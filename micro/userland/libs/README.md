# libs 用户态启动与系统调用包装

运行库提供最小程序入口和类型化的系统调用包装。入口把预置的启动参数交给应用。包装函数把调用者手里的句柄和消息值编成 RV64 寄存器输入，再解出返回状态和结果。

## 最小模型

```text
Program = (start_info, phase)
Call    = (nr, a0..a5)
Result  = (status, meta, words[4])

ENTRY ──_start──> IN_APP ──app_main 返回──> THREAD_EXIT
ARGS  ──编码──> CALL ──ecall──> RETURN ──解码──> RESULT
```

| 操作 | 契约 |
| --- | --- |
| `_start(start_info)` | 使用预置用户栈调用 `app_main(start_info)`，应用返回后请求结束当前线程 |
| `syscall(nr, args)` | 传入 `a7` 和 `a0..a5`，把返回寄存器交给上层解码 |
| `ipc_send / ipc_reply` | 编码句柄或回复令牌、标签和四个消息字，返回状态 |
| `ipc_recv / ipc_call` | 返回状态，成功时从 `meta` 和四个消息字组装消息 |

寄存器分配和字段位宽取自模型 ABI。 [ABI 最小模型](../../shared/abi/README.md)

## 不变量

1. 进入 C 函数时 `sp` 保持 16 字节对齐。 [RISC-V 整数调用约定](https://docs.riscv.org/reference/abi/v1.0/riscv-cc-procedure-calling-convention.html)
2. 句柄和令牌按 32 位无符号值编码，状态按 64 位有符号值解码。
3. 调用前将待发送消息复制到本次的参数寄存器。只有成功返回时才交付接收消息，错误时调用方仍持有原输出内容。

## 独立验证

用寄存器替身截获 `ipc_send`，核对句柄、标签和四个消息字。让替身依次返回有效应答和错误码，检查消息重建、错误传播及输出内容。

## QEMU demo

`runtime.c` 的 `user_start` 在预置用户栈上调用应用入口，再发出 `THREAD_EXIT`。IPC 包装函数通过真实 U-mode `ecall` 与测试陷阱处理器交换寄存器。

```sh
make -C micro/userland/libs run
```

从仓库根目录运行，成功时输出 `[libs] PASS` 并退出 QEMU。
