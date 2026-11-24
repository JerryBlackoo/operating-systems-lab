# syscall 系统调用分发

系统调用层把一次 U-mode `ecall` 解码成一个内核原语调用，再把结果写回发起调用的陷阱帧。它负责分发、参数校验和状态码转换。线程状态、页表和端点队列各由所属模块维护。

## 最小模型

```text
Table[nr] = (参数形状, 校验规则, handler)
Call      = (caller, frame, nr, args[6], state)

ENTERED ──拒绝──> COMPLETE
    │
    └──校验通过──> DISPATCHED ──同步完成──> COMPLETE
                       │
                       ├──阻塞──> WAITING ──后端完成──> COMPLETE
                       └──退出线程──> EXITED
```

| 阶段 | 操作 |
| --- | --- |
| 解码 | 接收同步 U-mode `ecall` 的帧，从 `a7` 读调用号，从 `a0..a5` 取参数副本 |
| 校验 | 先确认编号和参数形状，再用可信的调用者身份检查句柄、对象类型和操作权限 |
| 分发 | 用后端执行 `YIELD/THREAD_EXIT`、`AS_MAP/AS_UNMAP` 或 `IPC_SEND/RECV/CALL/REPLY` |
| 完成 | 将后端结果转换成 ABI 状态码，写回原调用帧的 `a0..a5`。线程退出走 `EXITED` |

调用号、参数位宽和结果寄存器以同一份模型约定为准。 [ABI 最小模型](../../shared/abi/README.md)

## 不变量

1. 只有 `scause = 8` 的同步 U-mode 环境调用进入分发。陷阱帧的 `sepc` 指向当前 `ecall`，受理后推进 4 字节一次。参数错误也从下一条指令继续。 [RISC-V Supervisor ISA](https://docs.riscv.org/reference/isa/v20260120/priv/supervisor.html) · [ECALL 指令编码](https://docs.riscv.org/reference/isa/v20260120/unpriv/rv32.html)
2. 参数全部来自寄存器副本。映射请求中的地址供内存后端校验和使用，IPC 载荷是四个值。
3. 句柄在调用者的能力空间内解析。授权通过后，在途操作持有必要的对象引用。回复令牌只允许获授的接收者使用一次。 [capability 模型](../capability/README.md) · [IPC 模型](../ipc/README.md)
4. 未知编号或非法参数直接完成错误返回，后端调用次数为零。对已进入后端的请求，阻塞和唤醒共享同一次调用，完成结果只写回原调用帧一次。

## 独立验证

构造陷阱帧、调用者和带计数的模拟后端。用已知编号验证参数传递，用未知编号、过期句柄和未对齐地址验证拒绝。让 `IPC_CALL` 先进入 `WAITING`，再注入回复，检查分发次数为 1、`sepc` 只加 4、输出寄存器与回复一致。帧字段可直接参照架构模块。 [trap.h](../arch/riscv/trap.h)

## QEMU：统一 trap 上的最小真实闭环

这一节是本次新增的接线，和上面的“模型用例”、以及 `capability/ipc/sched/memory` 的**模块 demo** 分开看。

### 架构核实结论

- `micro/Makefile` 的正式镜像**链接** `shared/demo.c` 提供 `trap_handler`，**不链接** `kernel/arch/riscv/trap.c`；后者只属于 `kernel/arch/riscv/Makefile` 的 `ARCH_SELFTEST` 自检镜像。
- 因此“正式 trap 的统一 glue”必须落在 `shared/demo.c`，而不是再写一个只在某个 demo 生效的平行 handler。
- QEMU virt 上 S 态定时器必须通过 SBI `set_timer` 写 CLINT 比较器；`clock` 的 one-shot 队列和 `sched_start_tick()` 已封装这一点，但此前只有 `sched/demo.c` 在 trap 里把它们接起来。
- IPC 消息布局沿用 `kernel/ipc/ipc.h` 的 `tag(4) + reply_id(4) + words[4]` 共 40 字节；ABI 只在 meta 高 32 位暴露 reply 字段。本次闭环不修改布局，服务端从 `recv` 的 meta 高 32 位取内核签发的一次性 reply token。

### 最小闭环

```text
S 态 timer 中断
  -> shared/demo.c: 模块 trap 先观察（clock/interrupt 可接管）
  -> 默认 glue: clock_handle_interrupt() -> sched_tick()
                sched_check() -> sched() -> cpu_switch() -> as_activate(satp)

U 态 ecall
  -> 同一个 shared/demo.c trap_handler
  -> active_module->trap = kernel/syscall/closure.c
  -> syscall_dispatch()
  -> SEND/RECV: capability 句柄检查
     REPLY:     内核签发的单次 bearer token（ep_reply 校验存活/单次）
```

闭环由 `closure.c` 组装，只补连接，不重写模块：

| 新增接线 | 位置 |
| --- | --- |
| 正式 trap 的默认 timer glue（弱符号，未链接 clock/sched 的镜像归一化为空） | `shared/demo.c` |
| 内核根页表 + 直接映射窗口的恒等映射（PTE 标记为非拥有） | `kernel/memory/as.c: as_map_kernel_range()` |
| 用户线程首次从 S 态 `sret` 到 U 态的裸 trap frame | `kernel/syscall/closure.c` |
| 从用户线程切回内核线程时 `satp -> bare` | `kernel/sched/sched.c` |
| U 态程序模板（拷进用户代码页，两个 AS 共享） | `kernel/syscall/user_prog.S` |
| 模块 trap 只消费 U `ecall`，timer 帧交给统一 glue | `kernel/syscall/demo.c` |
| legacy 全链路的 `kernel_backend.ctx = &kernel`（此前缺失） | `kernel/syscall/demo.c` |

**timer glue 的边界**：默认 `BOOT_MODULES=capability` 镜像没有调用
`sched_start_tick()`，所以不会产生周期 tick；它只会在 clock 队列有 timer 时
走统一 glue。`clock`/`sched`/`interrupt` 的模块 demo 仍可先在
`active_module->trap` 里接管 timer（分别验证定时器队列、抢占和 irq 层），
未接管时才由 `shared/demo.c` 的默认 glue 执行。

### 验收证据（真实 QEMU virt）

完整串口记录见 [`qemu-unified.log`](qemu-unified.log)：

```sh
make -C micro run BOOT_MODULES=syscall
# exit code: 0
```

- **U-mode ecall 走同一 trap**：日志中的 `[umode] first ecall` 显示
  `scause=0x8`、`SPP=0`；`sepc` 是当时最早进入 U 态 ecall 的指令地址。
  具体值取决于哪个用户线程先 ecall（见 `qemu-unified.log`）。
- **两个 AS 的真实 satp 切换**：
  `satp switch 0x0 -> 0x8000000000080258 root=0x80258 at timer U-A`，
  随后 `... -> 0x800000000008025e root=0x8025e at timer U-B`。两个 root pfn 不同。
- **A 可写的地址在 B 中不可用**：
  启动前先用 `as_translate()` 证明 A 的 `0x40200000` 翻译成功且 `U/R/W`，
  B 的同一 VA 返回 `MEM_EFAULT`（日志 `[iso] precheck A_mapped=1/B_unmapped=1`）；
  A 在该页写入并读回 `0x5a5a5a5a5a5a5a5a`；
  B 访问同一 VA 时留下 `scause=0xd sepc=0x400001e0 stval=0x40200000 satp=0x800000000008025e`
  的 load page fault。PASS 条件还要求该 fault 来自 U-B、`scause` 必须是
  load page fault、`stval` 是该 VA、`satp` 等于 B 的 root；其它 U 态缺页
  只增加 `unexpected_errors`，不会置 `fault_seen`。
- **双方 U-mode IPC**：`U-B recv enter` 先阻塞，`U-A call enter tag=0x101` 唤醒它，
  B 的 `recv woke tag=0x101` 与 `reply sent tag=0x102`，最后 A 收到
  `call reply tag=0x102 word0=0x5a5a5a5a5a5a5a5b`。内核在 CALL 完成时校验
  tag/word0 后才置 `call_ok`。SEND/RECV 走 capability 句柄检查；REPLY 不走
  capability，而是使用 `ep_recv` 返回的单次 bearer token，由 `ep_reply()`
  校验存活/单次语义。
- **错误路径被记录**：
  `[err] U-B illegal syscall nr=99 -> status=0xffffffffffffffff expected`；
  内核要求 `illegal_calls == 1`、来源为 U-B、status 为 `ABI_BAD_NR`，且
  `unexpected_errors == 0`。U-B 汇编在 `nr=99` 后检查 `a0 == -1`，否则直接走 fail。
- **调度计数**：本次运行 `sched_ticks=32`、`sched_switches=12`、
  `quantum_expired=6`，来自真实 timer/SBI 路径（具体数值随调度时序变化）。

**日志解释**：

- `satp switch ... -> 0x0` 是切到 `as == NULL` 的内核线程时观察到的 satp；
  它不一定由该条 `at timer` 日志对应的 timer 抢占“造成”，而是上一次
  调度已经完成的页表状态。
- 非法 syscall 的 `sepc=0x400001d8` 是 `syscall_dispatch()` 受理后
  `sepc += 4` 指向的“检查 `a0 == -1`”指令；
  隔离缺页的 `sepc=0x400001e0` 是检查之后的 `ld`。旧版日志中两者同为
  `0x400001d8`，是因为当时 ecall 的下一条恰好就是 `ld`；为在 U 态断言
  `ABI_BAD_NR` 增加检查后，两者相差 8 字节。

### 为什么选 `BOOT_MODULES=syscall`

默认 `BOOT_MODULES=capability` 仍只运行能力模块自身的 S 态组合 demo，尚未挂上闭环。
`syscall` 的 `MODEL_SUPPORT_syscall` 已经包含 `capability/ipc/memory/sched/clock`，
所以它是最小必需组合，且仍然由 `micro/Makefile` 链接正式启动路径和
`shared/demo.c` 的 `trap_handler`。后续可把 `closure_run()` 挂到默认镜像，
不需要再新增一次性 demo 清单。

### 保留的既有 demo

- `model_cases()`：仍是假后端，只验证分发状态机。
- legacy 单 AS U-mode 用例：仍保留，用于对照此前 `satp=0` 下 capability + IPC + map/unmap 的既有接线。
- `sched` / `clock` / `interrupt` 的模块 demo 仍可自行接管 timer（它们分别验证抢占、定时器队列和 irq 层）；统一 glue 是它们的兜底，闭环不依赖这些模块的局部代码。

### 已知边界

- 用户程序是一个 PC 相对、被拷进物理帧的小汇编模板；拷贝后执行 `fence.i`，
  但它不是通用 ELF 加载器。
- `kernel_as` 是永久对象：闭环结束不调用 `as_unref(kernel_as)`，也不依赖
  `as_destroy` 回收它；帧池在下一次启动时由 `frame_init()` 重新初始化。
- reply 在闭环里走 ipc 模块签发的单次 bearer token；capability 的 reply cap
  路径由 `kernel/capability/demo.c` 单独覆盖。
- 闭环的负向访问会终止 B 线程；整体 `PASS` 表示“错误被拒绝并记录”，不是
  “非法访问成功”。