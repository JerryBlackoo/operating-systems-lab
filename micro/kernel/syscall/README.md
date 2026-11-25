# syscall 系统调用分发

系统调用层把一次 U-mode `ecall` 解码成一个内核原语调用，再把结果写回发起调用的陷阱帧。它负责分发、参数校验和状态码转换。线程状态、页表和端点队列各由所属模块维护。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct syscall_entry` | `syscall.c` | 分发表的一行：`argc`（有效参数个数）、`tag`（a1 是 32 位标签）、`result`（是否回写 a1..a5）、`kind`/`rights`（首句柄的对象类别与所需权限）、`valid`（附加校验）、`handle` |
| 分发表 `table[]` | `syscall.c` | 以 `enum abi_number` 为下标的稀疏数组；空洞（12/13/18）的 handler 为 NULL，`entry_for` 会当作未知编号 |
| `struct syscall_request` | `syscall.h` | 一次调用的**参数快照**：`caller`（可信的 `cap_sid`）、`nr`、`args`（a0..a5 副本）、`refs[2]`（acquire 得到的在途引用） |
| `struct syscall_call` | `syscall.h` | 一次调用的**完整状态**：`phase`、`request`、`frame`、`backend`、`held`、`waiting_thread`、`async_reply_id`、`async_ipc_done`/`status`/`result` |
| `enum syscall_phase` | `syscall.h` | `IDLE` / `IGNORED` / `ENTERED` / `DISPATCHED` / `WAITING` / `COMPLETE` / `EXITED` |
| `struct syscall_backend` | `syscall.h` | 三个原语缝：`acquire`（解析句柄并 pin）、`release`（撤销 pin）、`invoke`（执行语义） |
| `user_syscall_backend` | `user_backend.c` | resident 的唯一后端实现；单模块 demo 用 `fake_backend` |

`struct syscall_call` 存放在 `thread.ucall_words[24]` 里（`resident_trap.c` 有
`_Static_assert` 锁定尺寸）。这就是「每个线程一次只能有一个在途 syscall」的
物理依据。

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `arch/riscv`（trap） | 入口 | `shared/demo.c` 的 trap glue 在 `EXC_ECALL_U` 时调 `resident_user_trap(frame)` |
| `boot/resident_trap` | 调用者 | 取 `sched_current()->ucall_words` 作为 `syscall_call`，调 `syscall_dispatch`，再按 phase 决定 `thread_exit` / `syscall_wait_park` |
| `capability` | 句柄解析 | `user_backend_acquire` 走 `cap_acquire`，把 `cap_object` 存进 `backend_ctx->refs[]`；`release` 走 `cap_release` |
| `ipc` | 后端语义 | `ABI_IPC_*` 的 invoke 分派到 `ep_send`/`ep_recv`/`ep_call`/`ep_reply_from` |
| `memory` | 后端语义 | `ABI_AS_MAP`/`ABI_AS_UNMAP` → `as_map`/`as_unmap`；`ABI_BLOCK_READ`/`ABI_CONSOLE_*` 用 `as_copyin`/`as_copyout` |
| `sched` | 阻塞与唤醒 | `syscall_wait_park` 用 `self->syscall_wq`；`syscall_resume` 用 `thread_wake_locked` |
| `shared/abi` | 调用号与状态码 | `enum abi_number`、`enum abi_status`、`abi_result` / `abi_decode_*` |
| `pager` | 缺页与 syscall 的交界 | 缺页不走 syscall 分发（在 trap 里直接 `pager_handle_user_fault`），但 `ABI_VM_MAP`/`ABI_PAGER_*` 是 VM 服务的 syscall |
| `kernel_spawn` / `storage` / `console` | 特权调用实现 | `ABI_PM_*`→`kernel_pm_*`；`ABI_BLOCK_*`→`kernel_storage_*`；`ABI_CONSOLE_*`→内核 console |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 裸 switch | 一个 `switch (a7)` 直接调各模块 | 单模块 demo 可用，但每个调用都自己重复参数校验 |
| 2. 表驱动分发 | `struct syscall_entry table[]` + `handle_none`/`handle_handle`/`handle_map` 三类 handler | 新增调用只需加一行，参数形状与句柄解析统一 |
| 3. 参数形状校验 | `shape_ok`：尾寄存器必须为 0、句柄/标签必须 32 位、`AS_MAP` 的 a2 也是 32 位 | 内核不能相信用户寄存器的高位；形状错误必须在触达后端前拒绝 |
| 4. 空洞防护 | `entry_for` 除了范围检查还要判 `table[nr].handle != 0` | designated initializer 留下的空槽曾让 `dispatch` 直接调 NULL handler，把整个内核打崩 |
| 5. 后端抽象 | `struct syscall_backend` 的 acquire/release/invoke | 让同一套分发逻辑既能在单模块 demo（fake backend）跑，也能在 resident（真实 cap/ipc/memory）跑 |
| 6. 阶段机 | `enum syscall_phase` + `finish()` | 阻塞调用需要「保持调用帧有效、稍后再写回」，`COMPLETE`/`EXITED`/`WAITING` 必须显式区分 |
| 7. `WAITING` 停泊 | `syscall_wait_park` + `syscall_resume` | 后端返回 `WAITING` 时 `sepc` 已推进但结果还没写回；必须阻塞在 trap 里等续完 |
| 8. 异步 IPC（A4） | `ep_call_open`/`deliver` 在 invoke 里完成，返回 `WAITING`；`ep_reply` 经 `user_syscall_complete_async_ipc` → `syscall_resume` 写回 | 让调用者不再占用内核栈同步等待；`MICRO_SYSCALL_ASYNC_IPC=0` 保留同步回退 |
| 9. 早完成缓存 | `async_ipc_done` / `async_ipc_status` / `async_ipc_result` + `syscall_drain_async_done` | `ep_reply` 可能早于 `WAITING` 落地；不缓存就会丢结果 |
| 10. park/resume 同锁 | `syscall_resume` 全程持 `sched_lock`；`syscall_wait_park` 在真正 block 前复查 phase | 否则「回复已到却仍挂在 `syscall_wq`」会死锁 |
| 11. 延迟终止兑现点 | `syscall_wait_park` 返回前检查 `sched_thread_kill_pending` | 被请求终止的线程必须在确定的点退出，而不是继续执行用户代码 |
| 12. 策略层迁出（A1） | 删除 `ABI_VFS_READ_AT`（编号 13 留空洞），新增 `ABI_BLOCK_READ`(27) / `ABI_BLOCK_INFO`(28) | ext4/vfs 迁到 U-mode 后，内核不再解释路径；只暴露原始块访问 |

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
- QEMU virt 上 S 态定时器必须通过 SBI `set_timer` 写 CLINT 比较器；`clock` 的 one-shot 队列和 `sched_start_tick()` 已封装这一点。本次改动前，默认 capability 镜像没有武装周期 tick；只有 `syscall` 闭环和 `sched/demo.c` 在显式配置下把它们接进 trap。现在默认镜像的抢占接线在 `kernel/boot/preempt_probe.c`，见 [`boot/README.md`](../boot/README.md)。
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

**timer glue 的边界（本次更新）**：默认 `BOOT_MODULES=capability` 现在会在
模块清单之后由 `kernel/boot/preempt_probe.c` 调用 `sched_start_tick()`，用两条
内核 worker 验证 timer 抢占，见 [`boot/qemu-default-preempt.log`](../boot/qemu-default-preempt.log)。
`syscall` 闭环仍独立调用 `sched_start_tick()`，并在同一 tick 窗口里运行 U-mode
服务；`clock`/`sched`/`interrupt` 的模块 demo 仍可先在 `active_module->trap`
里接管 timer（分别验证定时器队列、抢占和 irq 层），未接管时才由
`shared/demo.c` 的默认 glue 执行。

### 验收证据（真实 QEMU virt）

完整串口记录见 [`docs/evidence/qemu-unified-legacy.log`](../../docs/evidence/qemu-unified-legacy.log)（原 `qemu-unified.log`，已归档）：

```sh
make -C micro run BOOT_MODULES=syscall
# exit code: 0
```

- **U-mode ecall 走同一 trap**：日志中的 `[umode] first ecall` 显示
  `scause=0x8`、`SPP=0`；`sepc` 是当时最早进入 U 态 ecall 的指令地址。
  具体值取决于哪个用户线程先 ecall（见 `docs/evidence/qemu-unified-legacy.log`）。
- **两个 AS 的真实 satp 切换**（数值以 `docs/evidence/qemu-unified-legacy.log` 为准）：
  `satp switch 0x0 -> 0x8000000000080262 root=0x80262 at timer U-A`，
  随后 `... -> 0x8000000000080268 root=0x80268 at timer U-B`。两个 root pfn 不同，
  pfn 数值会随帧分配顺序变化。
- **A 可写的地址在 B 中不可用**：
  启动前先用 `as_translate()` 证明 A 的 `0x40200000` 翻译成功且 `U/R/W`，
  B 的同一 VA 返回 `MEM_EFAULT`（日志 `[iso] precheck A_mapped=1/B_unmapped=1`）；
  A 在该页写入并读回 `0x5a5a5a5a5a5a5a5a`；
  B 访问同一 VA 时留下 `scause=0xd sepc=0x400001e0 stval=0x40200000 satp=0x8000000000080268`
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
- **调度计数**：本次运行 `sched_ticks=31`、`sched_switches=23`、
  `quantum_expired=5`，来自真实 timer/SBI 路径（具体数值随调度时序变化）。
  计数也包含同一次 tick 窗口里的有限 U-mode sm 取证调度。

**日志解释**：

- `satp switch ... -> 0x0` 是切到 `as == NULL` 的内核线程时观察到的 satp；
  它不一定由该条 `at timer` 日志对应的 timer 抢占“造成”，而是上一次
  调度已经完成的页表状态。
- 非法 syscall 的 `sepc=0x400001d8` 是 `syscall_dispatch()` 受理后
  `sepc += 4` 指向的“检查 `a0 == -1`”指令；
  隔离缺页的 `sepc=0x400001e0` 是检查之后的 `ld`。旧版日志中两者同为
  `0x400001d8`，是因为当时 ecall 的下一条恰好就是 `ld`；为在 U 态断言
  `ABI_BAD_NR` 增加检查后，两者相差 8 字节。

### M3 取代的 U-mode SM 取证（历史）

2026-11-24 之前，`closure.c` 曾在 `closure_run()` 末尾附带一条 bare-satp 的
U-mode SM 探针（`[usvc]` 日志）。**M3 起已删除该路径**；真实 cap 拒绝与
init/sm/pm 服务组合见：

- [`docs/evidence/M3-2026-11-24-run.log`](../../docs/evidence/M3-2026-11-24-run.log)
- 历史串口：[`docs/evidence/qemu-unified-legacy.log`](../../docs/evidence/qemu-unified-legacy.log)

`closure_run()` 现只保留双 AS 的 IPC call/reply、非法 ecall 与跨 AS 缺页隔离。

### 为什么选 `BOOT_MODULES=syscall`

`MODEL_SUPPORT_syscall` 含 `capability/ipc/memory/sched/clock`，承载
`closure_run()` 的双 AS 取证闭环，仍由 `micro/Makefile` 与 `shared/demo.c`
的 `trap_handler` 链接。

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
- **默认 capability 镜像**：`user_backend` 为独立 U-mode ELF（init/sm/pm/fs/
  console/shell/vm/…）提供真实 syscall；`loader_spawn_user` 负责 satp 与设备
  MMIO 映射。单模块 `syscall` demo 仍用汇编模板对照双 AS 取证。
- `kernel_spawn.c` 仅 PM 调用 `ABI_PM_SPAWN` 时运行；init `plan=3` 只 spawn
  fs/console/shell（vm/heaptest 由内核 spawn）。
- **resident 异步 IPC**（默认）：`ABI_IPC_CALL` 走 `ep_call_open` / `ep_call_deliver`，
  发起线程在 `syscall_wait_park` 里阻塞，由 `ep_reply` 经
  `user_syscall_complete_async_ipc` → `syscall_resume` 写回 trap 帧并唤醒。
  同步回退：`make MICRO_SYSCALL_ASYNC_IPC=0`（此时 `ep_call` 在内核里 sched 等待）。
- 异步路径的关键不变量：`ep_call_open` 的**创建引用**必须由
  `ep_reply`、`ep_call_cancel_async` 或 endpoint 销毁路径归还；否则
  `IPC_MAX_REPLIES`（16）耗尽后所有 `IPC_CALL` 返回 `ABI_NO_MEMORY`。
- `syscall_resume` 与 `syscall_wait_park` 共用 `sched_lock`：resume 在锁内
  落 `COMPLETE` 并 `thread_wake_locked`；park 在真正 block 前复查 phase，
  避免「回复已到却仍挂在 `syscall_wq`」。
- 没有 per-thread `syscall_call`（`ucall_words` 为空）时，异步分支退回同步
  `ep_call`，绝不「投递了却没人等回复」。

## M7 新增调用（resident）

| nr | 名称 | 参数 | 权限与语义 |
| --- | --- | --- | --- |
| 9 | `ABI_CAP_MINT_TO` | `a0=src,a1=to_sid,a2=rights,a3=badge` | **仅 `SERVICE_SID_SM`**；跨 SID 需源 cap 带 `GRANT` |
| 10 | `ABI_PM_SPAWN` | `a0=parent,a1=image,a2=reserved` | **仅 `SERVICE_SID_PM`**；安装固定 cap 并加载 initrd ELF |
| 11 | `ABI_CAP_DELETE` | `a0=handle` | 删除调用者自己 SID 里的 cap |
| 13 | ~~`ABI_VFS_READ_AT`~~ | — | **已删除（2026-11-25，A1）**：ext4/vfs 迁到 U-mode，内核不再解释路径。编号留作空洞 |
| 14 | `ABI_CONSOLE_READ` | `a0=ubuf,a1=max` | **仅 `SERVICE_SID_CONSOLE`**；非阻塞，空队列 `ABI_BUSY`；shell 经 console 服务 IPC 取行 |
| 15 | `ABI_CONSOLE_WRITE` | `a0=ubuf,a1=len` | **仅 `SERVICE_SID_CONSOLE`**；走 `console_puts`（MMIO 优先），单次最多 128 B |
| 16 | `ABI_POWEROFF` | — | **仅 shell**；打印 `[kmain] shutdown requested by shell` 后 `sbi_shutdown()` |
| 17 | `ABI_PID_SELF` | — | 返回当前 thread object id |
| 19 | `ABI_VM_MAP` | `a0=tid,a1=va,a2=rights` | **仅 `SERVICE_SID_VM`**；须 `pager_authorize_map(tid,va,as_id)` 票 **且** `cap_vm_can_map_as`（VM CSpace 持目标 AS 的 `MAP` cap） |
| 24 | `ABI_REPLY_CALLER_SID` | `a0=reply_id` | **仅 `SERVICE_SID_SM` 与 `SERVICE_SID_VM`**；返回 `ep_call` 发起者的内核 `cap_sid`。SM 用它做名称服务鉴权，VM 用它校验 `TAG_VM_FORGET` 确实来自 PM |
| 25 | `ABI_VM_GRANT_AS` | `a0=thread_id,a1=pm_as_cap` | **仅 `SERVICE_SID_PM`**；`a1!=0` 时用 PM 侧 `MAP\|GRANT` 源 cap `cap_mint_to` 减权给 VM 后删除源 cap；`a1==0` 仅 boot/诊断：`cap_vm_ensure_as_map` |
| 26 | `ABI_PM_DISCARD` | `a0=thread_id,a1=image` | **仅 `SERVICE_SID_PM`**；撤销子进程。READY/普通阻塞当场摘除，RUNNING 登记延迟退出（下次进入内核时退出），异步 park 先撤销在途 reply。仅 current/idle 返回 `ABI_BUSY` |
| 27 | `ABI_BLOCK_READ` | `a0=lba,a1=udst,a2=blocks` | **仅 `SERVICE_SID_FS`**；读原始逻辑块到调用者缓冲区（A1：策略层在 U-mode）。成功 `words[0]`=字节数 |
| 28 | `ABI_BLOCK_INFO` | — | **仅 `SERVICE_SID_FS`**；`words[0]`=容量（块数）`words[1]`=块大小。U-mode `fs` 挂 ext4 时用它校验超级块与 extent 边界 |
| 10 出参 | `ABI_PM_SPAWN` 成功时 | `words[0]=展示 pid`，`words[1]=thread object id`，`words[2]=PM 持有的 AS 源 cap`（供 `ABI_VM_GRANT_AS` a1） |
| 20 | `ABI_PAGER_DONE` | `a0=fault_tid` | **仅 VM**；唤醒缺页线程 |
| 21 | `ABI_AS_SELF` | — | 任意 U-mode 线程；返回自己 AS 的 root pfn |
| 22 | `ABI_PAGER_FAIL` | `a0=tid,a1=status` | **仅 VM**；拒绝缺页并结束 fault 线程 |
| 23 | `ABI_PAGER_READY` | — | **仅 VM**；宣告 pager 可接收缺页 |

`ABI_NOT_FOUND = -10`。resident 下内核日志不再发 S-mode `DEMO_PRINT` ecall，而是直接
`console_puts`；panic 路径固定走 SBI。