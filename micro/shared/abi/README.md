# abi 内核与用户态的二进制约定

最小模型约定一次 RV64 `ecall` 的输入、输出和错误表示。以下编号及位布局是 **v0 模型草案**。内核分发层和用户态包装层可以用同一组寄存器向量校验各自的编解码。

## 最小模型

### 寄存器约定

| 寄存器 | 调用输入 | 调用输出 |
| --- | --- | --- |
| `a7` | 64 位系统调用号 | 保留 |
| `a6` | 保留 | 保留 |
| `a0` | 第一个参数 | 有符号 64 位状态 |
| `a1` | 第二个参数 | 消息元数据 `meta` |
| `a2..a5` | 第三至第六个参数 | 四个 64 位消息字 `w0..w3` |

### 共享编解码

`abi.h` 把位布局和形状校验做成两侧共用的内联函数，避免分发层和包装层各写一份位运算：

| 接口 | 作用 |
| --- | --- |
| `abi_meta` / `abi_meta_tag` / `abi_meta_reply` | 打包和解包 `meta[63:32]=reply, meta[31:0]=tag` |
| `abi_message_args` / `abi_decode_message` | 输入向量与消息之间的转换 |
| `abi_result` / `abi_decode_result` | 内核侧构造结果向量 / 用户侧只解码成功结果 |
| `abi_decode_status` | 把 `a0` 解成有符号状态 |
| `abi_tail_zero` / `abi_arg_u32` | 未用参数置零、32 位字段零扩展检查 |
| `abi_page_aligned` / `abi_rights_valid` | 映射地址对齐、R/W/X 权限形状 |

内核的分发层（[`syscall.c`](../../kernel/syscall/syscall.c)）和用户态包装层（[`runtime.c`](../../userland/libs/runtime.c)）使用同一组函数。具体每个调用号的参数个数仍由分发层决定，ABI 层不解释调用语义。

### 操作

| 调用号 | 操作 | `a0..a5` 输入 | 成功结果 |
| --- | --- | --- | --- |
| 1 | `YIELD` | 全零 | 继续执行 |
| 2 | `THREAD_EXIT` | 全零 | 调用线程结束 |
| 3 | `IPC_SEND` | `ep, tag, w0, w1, w2, w3` | 消息已交付 |
| 4 | `IPC_RECV` | `ep` | 收到的消息 |
| 5 | `IPC_CALL` | `ep, tag, w0, w1, w2, w3` | 服务端应答 |
| 6 | `IPC_REPLY` | `reply, tag, w0, w1, w2, w3` | 应答已交付 |
| 7 | `AS_MAP` | `as, va, frame, rights` | 一页映射已建立 |
| 8 | `AS_UNMAP` | `as, va` | 一页映射已移除 |

`ep`、`as`、`frame` 是调用者能力空间中的不透明句柄。映射虚拟地址 `va` 按 4 KiB 对齐，`rights` 的低三位依次表示读、写、执行。可写映射同时要求读权限。 [memory 模型](../../kernel/memory/README.md)

`tag` 是 32 位消息类型。成功接收或收到应答时，`meta[31:0]` 为 `tag`，`meta[63:32]` 为 `reply`。只有接收一条待应答的调用请求时 `reply` 才非零，接收者用它应答一次。四个消息字按索引顺序对应 `a2..a5`。 [内核消息模型](../../kernel/ipc/README.md)

### 错误表示

| 值 | 名称 | 含义 |
| --- | --- | --- |
| -1 | `BAD_NR` | 未分配的调用号 |
| -2 | `BAD_ARG` | 参数形状、取值或地址对齐错误 |
| -3 | `BAD_HANDLE` | 无效句柄或无效回复令牌 |
| -4 | `DENIED` | 对象类型或授权与操作不符 |
| -5 | `NO_MEMORY` | 分配所需资源失败 |
| -6 | `CLOSED` | 端点已关闭 |
| -7 | `ALREADY_MAPPED` | 虚拟页已有映射 |
| -8 | `NOT_MAPPED` | 虚拟页无映射 |
| -9 | `BUSY` | 所需资源暂不可用 |

## 不变量

1. 32 位句柄、令牌和标签用零扩展传入，未用参数置零。
2. 成功状态为 0，错误为负数。错误或无消息结果时，`a1..a5` 均置零。
3. 编解码以协议固定位宽和位移为准。相同的寄存器向量在分发层和包装层解出相同的值。

## 独立验证

输入 `a7=3, a0=0x10002, a1=7, a2=11`，其余参数为零，应解出发送操作和 `Message(7, [11, 0, 0, 0])`。接收方看到 `a0=0, a1=0x0000000900000007` 时，应解出 `tag=7, reply=9`。用 `a0=0xffffffffffffffff` 检查负状态 `BAD_NR` 的解码。

demo 还检查：`meta` 两端边界（`0` 与 `0xffffffff`）、四个消息字的顺序、全部状态码往返、错误结果 `a1..a5` 清零、32 位字段高位非零被拒绝、页对齐和 R/W/X 权限形状。

## QEMU demo

`abi.h` 定义调用号、状态码、消息编解码、共享校验和 RV64 `ecall`。demo 在 U-mode 发起发送、接收和非法调用号，S-mode 替身用同一组 `abi_*` 函数核对寄存器向量。

```sh
make -C micro/shared/abi run
```

从仓库根目录运行，成功时输出 `[abi] PASS` 并退出 QEMU。工具链使用 Clang、`ld.lld` 或 `rust-lld`，以及 `qemu-system-riscv64`。

```sh
make -C micro/shared run     # 分别启动全部 13 个镜像
```
