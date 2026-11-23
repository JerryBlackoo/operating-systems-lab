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
| 分发 | 用模拟后端执行 `YIELD/THREAD_EXIT`、`AS_MAP/AS_UNMAP` 或 `IPC_SEND/RECV/CALL/REPLY` |
| 完成 | 将后端结果转换成 ABI 状态码，写回原调用帧的 `a0..a5`。线程退出走 `EXITED` |

调用号、参数位宽和结果寄存器以同一份模型约定为准。 [ABI 最小模型](../../shared/abi/README.md)

## 不变量

1. 只有 `scause = 8` 的同步 U-mode 环境调用进入分发。陷阱帧的 `sepc` 指向当前 `ecall`，受理后推进 4 字节一次。参数错误也从下一条指令继续。 [RISC-V Supervisor ISA](https://docs.riscv.org/reference/isa/v20260120/priv/supervisor.html) · [ECALL 指令编码](https://docs.riscv.org/reference/isa/v20260120/unpriv/rv32.html)
2. 参数全部来自寄存器副本。映射请求中的地址供内存后端校验和使用，IPC 载荷是四个值。
3. 句柄在调用者的能力空间内解析。授权通过后，在途操作持有必要的对象引用。回复令牌只允许获授的接收者使用一次。 [capability 模型](../capability/README.md) · [IPC 模型](../ipc/README.md)
4. 未知编号或非法参数直接完成错误返回，后端调用次数为零。对已进入后端的请求，阻塞和唤醒共享同一次调用，完成结果只写回原调用帧一次。

## 独立验证

构造陷阱帧、调用者和带计数的模拟后端。用已知编号验证参数传递，用未知编号、过期句柄和未对齐地址验证拒绝。让 `IPC_CALL` 先进入 `WAITING`，再注入回复，检查分发次数为 1、`sepc` 只加 4、输出寄存器与回复一致。帧字段可直接参照架构模块。 [trap.h](../arch/riscv/trap.h)

## QEMU demo

`syscall.c` 实现分发表、参数检查和阻塞调用的续完。demo 先注入陷阱帧验证状态机，再用真实 U-mode `ecall` 验证入口与返回。映射、端点和能力检查由本地替身驱动。

```sh
make -C micro/kernel/syscall run
```

从仓库根目录运行，成功时输出 `[syscall] PASS` 并退出 QEMU。
