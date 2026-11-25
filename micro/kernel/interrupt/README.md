# 微内核中断最小模型

> 路径：`micro/kernel/interrupt/`  
从仓库根目录用 `make -C micro run BOOT_MODULES=interrupt` 启动 QEMU demo。模拟控制器验证 claim、单次通知、确认和解绑。

这个模块的最小核不是「中断处理函数集合」，而是：

**每线三态的 IRQ 对象 + 一次性通知协议。**

内核只做 `识别 → 闸住 → 通知`；设备处理在 U-mode，`ack` 才解闸。Linux 的
`IRQF_ONESHOT + request_threaded_irq` 是这个模型在宏内核里的退化形态。

本目录只实现这个核。PLIC MMIO、trap 入口、IPC/调度、capability 都在边界外；
`irq_port` 是它们与模块之间唯一的接缝。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct irq_line` | `irq.c` | 一条中断线的状态：`state`（三态）、`sink`（接收者）、`stats`（计数） |
| `enum irq_state` | `irq.h` | `DISARMED`（屏蔽且无主）/ `ARMED`（已解闸，可投递）/ `ACTIVE`（已 claim，等待 ack） |
| `struct irq_sink` | `irq.h` | 上层处理者：`notify(self, line)` + 不透明 `ctx` |
| `struct irq_port` | `irq.h` | **唯一的硬件接缝**：`claim` / `complete` / `mask` / `unmask` / `intr_save` / `intr_restore` |
| `struct irq_stats` | `irq.h` | 观测计数：`fires` / `acks` / `bad_acks` / `spurious` / `coalesced` |
| 线表 `g_lines[]` | `irq.c` | 固定 `IRQ_LINE_MAX = 96` 条线的状态数组 |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `arch/riscv/plic.c` | **提供 `irq_port` 实现** | `plic_irq_port()` 返回 claim/complete/mask/unmask + SIE 保存恢复；`plic_hw_init()` 清优先级与阈值 |
| `console` | 一个 `irq_sink` | `console_uart_notify` 绑到 `UART0_IRQ`(10)：逐字节读 RBR，`\r`/`\n` 时 `flush_line` 入队并唤醒等待者，末尾 `irq_ack` |
| `shared/demo.c`（trap glue） | 中断入口 | S 态外部中断里 `kernel_irq_enter_devices()` → `irq_dispatch()` → `kernel_irq_leave_devices()`，再 `sched_check()` |
| `clock` | 定时器线与 PLIC 的交叉 | `port_complete(IRQ_TIMER)` 不写 PLIC claim/complete，而是重新 `sbi_set_timer`；`port_mask` 写 `UINT64_MAX` 撤防 |
| `sched` | 中断后的抢占点 | `irq_dispatch` 本身不调度；trap 返回前的 `sched_check()` 才切换 |
| `virtio_blk`（resident） | 轮询而非中断 | 驱动用 `virtio_blk_poll()` 读 ISR 状态位，未绑定 PLIC 线；`irq_dispatch` 只服务 UART |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯模型 | 内存里的假控制器，验证 claim / 单次通知 / ack / 解绑 | 先把「每线三态 + 一次性通知」的语义定下来，不碰硬件 |
| 2. `irq_port` 抽象 | 把 `claim/complete/mask/unmask/intr_save/intr_restore` 抽成端口 | 换平台只换端口；模块本身不含任何 MMIO 地址 |
| 3. 无主事件必须 complete | `irq_fire` 在 `state != ARMED` 或 `sink == NULL` 时仍调 `port->complete` | PLIC 的 source 在 claim 后不 complete 会被永久闸住；漏掉就是整条线死掉 |
| 4. 先 ACTIVE 再 notify | `l->state = IRQ_ACTIVE` 之后才调 `sink->notify` | notify 期间同一线的重复事件只能合并，不能重入 |
| 5. notify 允许同步 ack | 返回后不再读写 `l`（局部副本） | UART 的 notify 会在末尾直接 `irq_ack`，此时状态已回到 ARMED |
| 6. 拆除顺序：先 mask 再 complete | `irq_unbind` 与 `lines_teardown` 都按这个顺序 | 若先 complete 再 mask，仍为高的电平线会在屏蔽前重新 pending |
| 7. PLIC 优先级别忘了设 | `port_unmask` 若读到 `PLIC_PRIO(line) == 0` 就先写 1 | PLIC 源优先级为 0 时永远不会触发；这是最容易被忽略的失效原因 |
| 8. S 态 timer 的特殊路径 | `port_complete` / `port_mask` 对 `IRQ_TIMER` 特判 | timer 不在 PLIC 里；它的「complete」等价于重装比较器 |
| 9. `irq_dispatch` 预算上限 | `IRQ_DRAIN_BUDGET = 32` | 防止电平线在 handler 有 bug 时把 CPU 卡在 dispatch 循环里 |
| 10. 单核锁语义差异（已确认非缺陷） | `port_intr_restore` 在 `flags == 0` 时显式清 SIE；`sched_unlock` 则保持不动 | 两者只在各自模块的短临界区内成对使用，不会互相嵌套破坏外层状态 |

---

## 1. QEMU virt 事实基线

以下数值来自本机 QEMU 8.2.2 `-machine virt,dumpdtb` 实测，以及 RISC-V / PLIC 规范。
换板子必须重新从 DTB 和手册取值，不能照抄。

| 事实 | 值 | 来源 |
|---|---|---|
| 平台 / 时基 | `riscv-virtio,qemu`，timebase 10 MHz | QEMU DTB 实测 |
| PLIC | base `0x0c000000`，`ndev=95`；S-mode 外部中断 cause 9 | DTB + RISC-V Privileged Spec |
| PLIC 上下文 | hart0：M=0、S=1；S 上下文 enable `0x0c002080`，threshold/claim `0x0c201000/0x0c201004` | PLIC Spec + QEMU virt 约定 |
| 定时器 | CLINT `0x02000000`；S-mode timer cause 5；SBI `set_timer` 单次 | DTB + SBI Spec |
| 设备线 | UART0 `0x10000000` IRQ 10；RTC `0x101000` IRQ 11；virtio-mmio `0x10001000..0x10008000` IRQ 1..8 | QEMU DTB 实测 |
| 关键 CSR | trap 时 `sstatus.SIE=0`、`SPIE` 保存；`sip.SEIP` 在 S-mode 只读，靠 claim/complete 清；`stvec` direct | RISC-V Privileged Spec |
| 内存 | `0x80000000`，128 MiB | QEMU DTB 实测 |

PLIC 的两个实操点：

- 源优先级为 0 时永远不会触发；`unmask` 前必须确保 `priority > 0`。
- `sip.SEIP` 不能靠写 CSR 清；必须通过 PLIC 的 claim/complete 释放。

---

## 2. 线命名空间

```text
line 0      : S-mode timer（scause = 5）
line 1..95  : QEMU virt PLIC source ID
IRQ_NONE    : claim 无待处理源
```

PLIC 的 ID 0 表示“没有中断”，正好留给 timer；因此 line 与 PLIC source ID
可以直接一一对应，不需要 `irq_domain` 那样的动态映射。等接多控制器或 DT
不稳定硬件号时再引入映射层。

---

## 3. 三态与迁移

| 状态 | 含义 | 控制器侧 |
|---|---|---|
| `DISARMED` | 未绑定 | 屏蔽 / 闸住 |
| `ARMED` | 已绑定，可投递 | 解闸 |
| `ACTIVE` | 已 claim，等待 ack | 闸门由 claim 持有 |

| 当前 | 事件 | 动作（顺序很重要） | 次态 |
|---|---|---|---|
| `DISARMED` | `bind(l, sink)` | 记录 sink → `unmask(l)` | `ARMED` |
| `ARMED` | `fire` | claim 已持有 → `notify(sink)` | `ACTIVE` |
| `ACTIVE` | `fire` | 只增加 `coalesced`，不再 notify | `ACTIVE` |
| `ACTIVE` | `ack` | `complete(l)` → 解闸 | `ARMED` |
| `ARMED` / `ACTIVE` | `unbind` | `mask(l)` → 若 ACTIVE 再 `complete(l)` → 清 sink | `DISARMED` |
| 任意 | `reset` | mask all → complete 所有 ACTIVE → 清空线表并注销端口 | 未初始化 |

`fire` 里的“claim 已持有”是因为外部中断在 trap 入口先 claim 得到 line；
timer 由 `scause=5` 直接给出，不存在 PLIC claim。

---

## 4. API

| 函数 | 作用 | 调用上下文 |
|---|---|---|
| `irq_init(port)` | 清空线表、屏蔽所有线、登记端口；重入时先归还旧端口的 ACTIVE claim | 启动期，本地中断关闭 |
| `irq_reset()` | mask 所有线、complete 所有 ACTIVE、清空统计并注销端口 | 关闭/重启期；内部关中断 |
| `irq_bind(line, sink)` | 绑定 sink 并解闸；重复绑定返回 `IRQ_EBUSY` | 进程上下文；内部关中断 |
| `irq_unbind(line)` | 屏蔽、必要时 complete、清 sink；未绑定返回 `IRQ_ENOENT` | 进程上下文；内部关中断 |
| `irq_ack(line)` | `complete` 并回到 `ARMED`；非 ACTIVE 返回 `IRQ_EINVAL` 并计数 | 进程上下文 / 系统调用；内部关中断 |
| `irq_fire(line)` | 处理一条已知线的事件；返回 1=已通知，0=合并/伪中断 | trap 上下文，本地中断已关闭 |
| `irq_dispatch()` | 从 `port->claim()` 循环取线，最多 `IRQ_DRAIN_BUDGET` 次 | trap 上下文，本地中断已关闭 |
| `irq_get_state(line, &state)` | 读状态，不改变状态 | 任意；调试/测试 |
| `irq_get_stats(line, &stats)` | 读计数，不改变状态 | 任意；调试/测试 |
| `irq_state_name(state)` | 状态名 | 任意 |

错误码：`IRQ_OK=0`、`IRQ_EINVAL=-2`、`IRQ_EBUSY=-3`、`IRQ_ENOENT=-4`；
`IRQ_NONE=-1` 只用于 `port->claim()`。

运行原语只有两个：`irq_fire()` 和 `irq_ack()`。其余都是配置/观测。

---

## 5. 平台端口 `irq_port`

```c
struct irq_port {
    int  (*claim)(void);
    void (*complete)(int line);
    void (*mask)(int line);
    void (*unmask)(int line);
    unsigned long (*intr_save)(void);
    void (*intr_restore)(unsigned long flags);
};
```

| 操作 | 契约 |
|---|---|
| `claim()` | 返回下一条外部待处理线，或 `IRQ_NONE`；只用于外部中断 |
| `complete(line)` | 必须“解闸”：PLIC 写 complete，timer 重装；若该线已 mask，不得让它重新 pending |
| `mask(line)` | 只屏蔽一条线；不得睡眠；不得清掉已 claim 的 source |
| `unmask(line)` | 只解屏蔽一条线；若设备线仍为高，应使其中断变为 pending |
| `intr_save()` | 返回旧的中断状态，并关闭本地中断 |
| `intr_restore(flags)` | 原样恢复 |

端口实现必须保证：所有操作都能在关中断上下文中完成，不做阻塞、分配或用户内存访问。

### 5.1 接到 QEMU virt PLIC

**默认常驻镜像（M4）**：`kernel/arch/riscv/plic.c` 实现本节的 `irq_port`；
`kernel/console/console.c` 绑定 line `UART0_IRQ`（10）。

模块 demo 仍用 `test_irq.c` 主机 Mock。下文为示意（结构与 `plic.c` 一致）：

```c
static bool timer_masked = true;

static int port_claim(void)
{
    int id = plic_claim();          /* 读 0x0c201004 */
    return id == 0 ? IRQ_NONE : id; /* PLIC 0 = 无中断 */
}

static void port_complete(int line)
{
    if (line == IRQ_TIMER) {
        /* 已 mask 时不得重新 pending；否则 ack 才重装下一次。 */
        sbi_set_timer(timer_masked ? UINT64_MAX
                                   : csr_read(time) + TICK_INTERVAL);
    } else {
        plic_complete(line);        /* 写 0x0c201004 */
    }
}

static void port_mask(int line)
{
    if (line != IRQ_TIMER) {
        plic_enable_clear(line);    /* 写 0x0c002080 位图 */
    } else {
        timer_masked = true;
        csr_clear(sie, SIE_STIE);
        sbi_set_timer(UINT64_MAX);  /* SBI timer 是电平式，必须撤防 */
    }
}

static void port_unmask(int line)
{
    if (line != IRQ_TIMER) {
        plic_set_priority(line, 1); /* priority 0 永远不会触发 */
        plic_enable_set(line);
    } else {
        timer_masked = false;
        sbi_set_timer(csr_read(time) + TICK_INTERVAL);
        csr_set(sie, SIE_STIE);
    }
}
```

timer 是电平式的：trap 入口在调用 `irq_fire(IRQ_TIMER)` 之前必须先
`sbi_set_timer(UINT64_MAX)` 撤防，否则 ACTIVE 期间比较值仍在过去，
会形成中断风暴，而不是“被闸门合并”。

`intr_save/restore` 对应 `sstatus.SIE`：

```c
static unsigned long port_intr_save(void)
{
    unsigned long flags = csr_read(sstatus);
    csr_clear(sstatus, SSTATUS_SIE);
    return flags;
}

static void port_intr_restore(unsigned long flags)
{
    if (flags & SSTATUS_SIE) {
        csr_set(sstatus, SSTATUS_SIE);
    } else {
        csr_clear(sstatus, SSTATUS_SIE);
    }
}
```

trap 入口的接线：

```text
scause == 0x8000000000000005  ->  irq_fire(IRQ_TIMER)
scause == 0x8000000000000009  ->  irq_dispatch()
用户 ack 系统调用               ->  irq_ack(line)
```

启动顺序建议：

1. 关闭全局中断（`sstatus.SIE=0`）。
2. 初始化 PLIC：threshold=0、关闭全部 source enable。
3. `irq_init(&port)`：模块会把所有线置为 `DISARMED` 并调用 `mask()`。
4. `irq_bind()` 需要的线。
5. 打开 `sie.SEIE` 与 `sstatus.SIE`。

---

## 6. 构建与测试

```sh
cd micro/kernel/interrupt
make
make test
```

期望输出：

```text
ALL TESTS PASSED
```

本目录文件：

| 文件 | 说明 |
|---|---|
| `irq.h` | 对外 API、三态、统计、`irq_port` 契约 |
| `irq.c` | 三态机、bind/unbind/ack/reset、`irq_fire`、`irq_dispatch` |
| `test_irq.c` | 主机测试 + PLIC 语义 Mock（claim 闸门、电平重采样、伪中断） |
| `demo.c` | QEMU 集成：模拟控制器 + 真实 S-mode timer + IPC 驱动线程 |
| `Makefile` | 构建主机测试 |
| `.gitignore` | 忽略测试二进制 |

测试使用 Mock，不访问真实 MMIO；因此可以在普通 Linux 主机上 `make test`。

QEMU 集成 demo 分两段：

1. **模拟控制器**：验证 claim 闸门、单次通知、ack 解闸与 unbind。
2. **真实链路**：S-mode timer 中断 → trap 识别并撤防 → `irq_fire()` →
   sink 在中断上下文向已阻塞的驱动线程投递 IPC 消息 → 驱动线程
   `irq_ack()` 解闸（`complete` 重装 timer）。

```sh
make -C micro run BOOT_MODULES=interrupt
```

`MODEL_SUPPORT_interrupt` 因此链接 `objects/memory/sched/ipc`，但 `irq.c`
本身仍然只依赖 `irq_port`。

---

## 7. 测试覆盖

| 用例 | 验证点 |
|---|---|
| `test_uninitialized` | 未初始化时所有入口拒绝服务 |
| `test_init_and_bind` | 初始化屏蔽全部线；绑定/解绑/重复绑定/重复解绑 |
| `test_spurious` | 无主线事件计数并 complete，不 notify |
| `test_one_shot_level` | ACTIVE 期间不二次通知；ack 后电平重采样；先清设备再 ack |
| `test_sync_ack` | notify 内同步“清设备 → ack”可行；再次到达证明已解闸 |
| `test_reset` | reset 归还 ACTIVE claim、mask 全部线并注销端口 |
| `test_reinit_active` | 直接重入 init 会先 complete 旧 claim，再全量 mask |
| `test_reset_from_notify` | notify 内 reset：dispatch 立即停止 claim，不解引用已注销端口 |
| `test_unbind_active` | ACTIVE 中解绑会 complete；重绑时补 pending |
| `test_timer` | timer 独立于 PLIC；ack 重装；未绑定事件也会重装 |
| `test_multi_source_budget` | 40 条线分两次 drain；预算用完后由下一次 trap 继续 |
| `test_invalid` | 越界线号、空 sink、错误状态 ack、状态名 |

其中三组顺序最容易写错：

- ACTIVE 期间的新事件只能合并，不能再次 notify，也不能提前 complete。
- 未清设备就 ack，会立刻重新 pending；这是电平中断的正确行为，不是 bug。
- reset/重入 init 必须先 mask 再 complete，否则仍为高的电平线会在拆除窗口重新 pending。

---

## 8. 不变量

- **I1 独占**：一条线最多一个 sink。共享不是原语；要共享就在用户态放一个多路复用 server。
- **I2 一次性**：ACTIVE 期间不再 notify；新事件由控制器闸门合并，只计数。
- **I3 成对**：每次 claim 恰好一次 complete；伪中断路径也必须 complete。
- **I4 无主不投递**：`DISARMED` 线永不 notify。
- **I5 有界**：top half 不阻塞、不分配、不访问用户内存；drain 有预算。
- **I6 帧完整**：trap 入口在进入 C 之前保存完整 trap frame；top half 全程 `SIE=0`。
- **I7 顺序**：驱动先清设备源，再 ack/complete；unbind 先 mask 再 complete。
- **I8 可观测**：`fires / acks / spurious / coalesced / bad_acks` 计数，但不改变控制流。
- **I9 上半部非阻塞**：`notify` 在 `SIE=0` 的 trap 上下文运行；可以 `thread_wake()`
  或向“已经阻塞的接收者”投递 IPC，但绝不能进入会阻塞的路径。

---

## 9. 边界与下一步

本实现明确不包含：

- SMP / 多 hart、IPI、TLB shootdown；
- 边沿触发（ACTIVE 期间的脉冲需要设备锁存或软件 pending 位）；
- 中断共享、优先级抢占、affinity、MSI；
- capability 授权、真实 PLIC MMIO 后端；
- 设备寄存器语义（驱动负责）。

`demo.c` 的真实链路只接 S-mode timer：它验证 trap → `irq_fire` → sink →
IPC/线程 → `irq_ack` → `complete` 的完整对象协议。PLIC 的 claim/complete
语义由主机 Mock 覆盖；真实 PLIC 端口属于 `arch/riscv` 的机器服务层
（`platform.h` 已给出 `PLIC_BASE`），不应塞进 `irq.c`。

接入顺序建议：

1. **M0**：timer only，假 sink，验证 trap 帧与 one-shot。
2. **M1**：PLIC + UART0 IRQ 10 + 伪中断，验证 claim/complete 与电平重采样。
3. **M2**：把 `notify` 接到阻塞线程/endpoint；再加边沿、共享、优先级、SMP。

关键约束：

- `irq_fire()` / `irq_dispatch()` 必须在本地中断关闭的 trap 上下文调用；
- `irq_init()` 可重入：旧端口存在 ACTIVE 线时会先 mask 再 complete，再切换端口；
- `irq_reset()` 是拆除操作，成功后端口注销，必须重新 `irq_init()`；
- `irq_bind()` / `irq_unbind()` / `irq_ack()` 会在状态迁移期间自行关中断；
- `sink->notify()` 必须非阻塞，模块不负责唤醒或切换线程。

---

## 10. 参考

- Linux：`include/linux/irq.h`、`kernel/irq/irqdesc.c`、`kernel/irq/chip.c`、
  `kernel/irq/manage.c`、`drivers/irqchip/irq-sifive-plic.c`。
  重点看 `IRQF_ONESHOT` 与 threaded IRQ：它们是“handler 完成前保持 masked”的先例。
- seL4：`IRQControl_Get`、`IRQHandler_SetNotification`、`IRQHandler_Ack`。
- Zircon：`zx_interrupt_create`、`zx_interrupt_bind`、`zx_interrupt_wait`、`zx_interrupt_ack`。
- QNX：`InterruptAttachEvent`、`InterruptWait`、`InterruptUnmask`。
- HelenOS：IRQ 订阅 + IPC 通知驱动线程。
- L4/Fiasco：IRQ 对象 + IPC，“中断即 IPC”。
