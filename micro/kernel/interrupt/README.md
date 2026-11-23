# 微内核中断最小模型

> 路径：`micro/kernel/interrupt/`  
从仓库根目录用 `make -C micro run BOOT_MODULES=interrupt` 启动 QEMU demo。模拟控制器验证 claim、单次通知、确认和解绑。

这个模块的最小核不是“中断处理函数集合”，而是：

**每线三态的 IRQ 对象 + 一次性通知协议。**

内核只做 `识别 → 闸住 → 通知`；设备处理在 U-mode，`ack` 才解闸。Linux 的
`IRQF_ONESHOT + request_threaded_irq` 是这个模型在宏内核里的退化形态。

本目录只实现这个核。PLIC MMIO、trap 入口、IPC/调度、capability 都在边界外；
`irq_port` 是它们与模块之间唯一的接缝。

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
| 任意 | `reset` | 全局关中断 → mask all → complete 所有 ACTIVE | `DISARMED` |

`fire` 里的“claim 已持有”是因为外部中断在 trap 入口先 claim 得到 line；
timer 由 `scause=5` 直接给出，不存在 PLIC claim。

---

## 4. API

| 函数 | 作用 | 调用上下文 |
|---|---|---|
| `irq_init(port)` | 清空线表、屏蔽所有线、登记端口 | 启动期，本地中断关闭 |
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

示意（不在本目录编译，接入 `arch/riscv` 时使用）：

```c
static int port_claim(void)
{
    int id = plic_claim();          /* 读 0x0c201004 */
    return id == 0 ? IRQ_NONE : id; /* PLIC 0 = 无中断 */
}

static void port_complete(int line)
{
    if (line == IRQ_TIMER) {
        sbi_set_timer(csr_read(time) + TICK_INTERVAL);
    } else {
        plic_complete(line);        /* 写 0x0c201004 */
    }
}

static void port_mask(int line)
{
    if (line != IRQ_TIMER) {
        plic_enable_clear(line);    /* 写 0x0c002080 位图 */
    } else {
        csr_clear(sie, SIE_STIE);
    }
}

static void port_unmask(int line)
{
    if (line != IRQ_TIMER) {
        plic_set_priority(line, 1); /* priority 0 永远不会触发 */
        plic_enable_set(line);
    } else {
        csr_set(sie, SIE_STIE);
    }
}
```

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
| `irq.c` | 三态机、bind/unbind/ack、`irq_fire`、`irq_dispatch` |
| `test_irq.c` | 主机测试 + PLIC 语义 Mock（claim 闸门、电平重采样、伪中断） |
| `Makefile` | 构建测试 |
| `.gitignore` | 忽略测试二进制 |

测试使用 Mock，不访问真实 MMIO；因此可以在普通 Linux 主机上 `make test`。

---

## 7. 测试覆盖

| 用例 | 验证点 |
|---|---|
| `test_uninitialized` | 未初始化时所有入口拒绝服务 |
| `test_init_and_bind` | 初始化屏蔽全部线；绑定/解绑/重复绑定/重复解绑 |
| `test_spurious` | 无主线事件计数并 complete，不 notify |
| `test_one_shot_level` | ACTIVE 期间不二次通知；ack 后电平重采样；先清设备再 ack |
| `test_unbind_active` | ACTIVE 中解绑会 complete；重绑时补 pending |
| `test_timer` | timer 独立于 PLIC；ack 重装；未绑定事件也会重装 |
| `test_multi_source_budget` | 40 条线分两次 drain；预算用完后由下一次 trap 继续 |
| `test_invalid` | 越界线号、空 sink、错误状态 ack、状态名 |

其中 `test_one_shot_level` 覆盖了最容易写错的两条顺序：

- ACTIVE 期间的新事件只能合并，不能再次 notify，也不能提前 complete。
- 未清设备就 ack，会立刻重新 pending；这是电平中断的正确行为，不是 bug。

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

---

## 9. 边界与下一步

本实现明确不包含：

- SMP / 多 hart、IPI、TLB shootdown；
- 边沿触发（ACTIVE 期间的脉冲需要设备锁存或软件 pending 位）；
- 中断共享、优先级抢占、affinity、MSI；
- capability 授权、IPC/调度器、真实 PLIC MMIO 后端；
- 设备寄存器语义（驱动负责）。

接入顺序建议：

1. **M0**：timer only，假 sink，验证 trap 帧与 one-shot。
2. **M1**：PLIC + UART0 IRQ 10 + 伪中断，验证 claim/complete 与电平重采样。
3. **M2**：把 `notify` 接到用户线程；再加边沿、共享、优先级、SMP。

关键约束：

- `irq_fire()` / `irq_dispatch()` 必须在本地中断关闭的 trap 上下文调用；
- `irq_init()` 只用于启动期，不得在存在 ACTIVE 线时重入；它不会替调用者 complete 旧 claim；
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
