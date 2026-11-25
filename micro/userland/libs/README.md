# libs 用户态启动与系统调用包装

运行库提供最小程序入口和类型化的系统调用包装。入口把预置的启动参数交给应用。包装函数把调用者手里的句柄和消息值编成 RV64 寄存器输入，再解出返回状态和结果。

它是**每个** U-mode 服务与应用的唯一 syscall 入口——`user_ipc_call`、`user_cap_mint_to`、`user_block_read` 等都在这里。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `user_start` | `runtime.c` | 最小程序入口：把启动参数交给 `app_main`，返回后发 `ABI_THREAD_EXIT` |
| `struct abi_regs` 包装 | `runtime.c` | 每个 `user_*` 函数把参数编成 `a0..a5`、把 `a7` 设为调用号，再用 `abi_decode_*` 解结果 |
| `user_ipc_send` / `user_ipc_recv` / `user_ipc_call` / `user_ipc_reply` | `runtime.c` / `runtime.h` | IPC 四件套；`call` 是异步/同步两条内核对路径的统一入口 |
| `user_cap_mint_to` / `user_cap_delete` | `runtime.c` | 能力派生与删除（SM 用） |
| `user_reply_caller_sid` | `runtime.c` | 查某次在途 call 的真实发起者 `cap_sid`（SM/VM 用，A3/A19） |
| `user_block_read` | `runtime.c` | 原始块读（A1 后 U-mode `fs` 用） |
| `user_console_read` / `user_console_write` | `runtime.c` | UART 行输入/输出（console 服务用） |
| `user_pid_self` / `user_as_self` / `user_poweroff` / `user_vm_map` / `user_pager_done` / `user_pager_fail` / `user_pager_ready` / `user_cap_*` / `user_vm_grant_as` | `runtime.c` | 其余特权调用的类型化包装 |
| `user_print` / `user_print_u64` | `user_io.c` | 经 console 服务打印（不是直接写 UART） |

## 与其他模块的关联

| 对方模块 | 关系 |
| --- | --- |
| `shared/abi` | 用 `abi_message_args` / `abi_decode_result` / `abi_decode_status` 编解码；`enum abi_number` 决定 `a7` |
| `kernel/syscall` | 每个 `user_*` 对应分发表里的一行；形状错误由内核 `shape_ok` 拒绝 |
| 所有 U-mode 服务 | `sm`/`pm`/`fs`/`console`/`vm`/`shell`/`init`/`heaptest` 全部链接本模块 |
| `userland/libs/service_proto.h` | 只引用 `shared/abi/service.h`（A22），不再维护 tag/SID 副本 |
| `userland/libs/user_io.c` | `user_print` 走 `TAG_CONSOLE_WRITE` 到 console 服务，因此服务日志也受 24 B 分块约束 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 裸 ecall | 手工写 `register` 约束 + `ecall` | 单模块 `libs` demo 先证明寄存器约定 |
| 2. 类型化包装 | `user_ipc_send/recv/call/reply` 各自封装 | 每个调用点手写寄存器太容易错位 |
| 3. 栈对齐 | `user_start` 保证 `sp` 16 字节对齐 | RISC-V ABI 要求；不对齐会让编译器生成的向量/浮点指令崩 |
| 4. 符号扩展的状态 | `abi_decode_status` 统一把 `a0` 解成 `int64_t` | 早期用 uint64 比较负值永远失败 |
| 5. 按需增补包装 | `user_cap_mint_to`、`user_reply_caller_sid`、`user_block_read` 等都随内核新增 ABI 而加 | 每加一个 ABI 就要在两侧各加一层，`abi.h` 共享编解码把位运算收敛到一处 |
| 6. `user_print` 走 IPC | 不再直接 `ABI_CONSOLE_WRITE`（那是 console 服务专属） | 只有 console SID 能碰 UART；其它服务必须经 console 服务 |
| 7. `user_block_read`（A1） | 取代已删除的 `user_vfs_read_at` | 策略层迁到 U-mode 后，fs 服务需要的是原始块访问 |

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
