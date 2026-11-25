# abi 内核与用户态的二进制约定

最小模型约定一次 RV64 `ecall` 的输入、输出和错误表示。以下编号及位布局是 **v0 模型草案**。内核分发层和用户态包装层可以用同一组寄存器向量校验各自的编解码。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `enum abi_number` | `abi.h` | 调用号；从 1 到 `MICRO_ABI_MAX_NR`，其中 12/13/18 是空洞 |
| `enum abi_status` | `abi.h` | 负值状态码：`ABI_BAD_NR`、`ABI_BAD_ARG`、`ABI_DENIED`、`ABI_NO_MEMORY`、`ABI_CLOSED`、`ABI_BUSY`、`ABI_NOT_FOUND`、`ABI_TIMED_OUT` 等 |
| `struct abi_regs` | `abi.h` | `uint64_t a[6]`，即 `a0..a5` 的向量；`_Static_assert` 锁定 6 个字 |
| `struct abi_message` | `abi.h` | `tag` + `reply` + `words[4]`（32 字节）；与 `ipc_message` 布局一致但独立定义 |
| 权限常量 | `abi.h` | `ABI_READ` / `ABI_WRITE` / `ABI_EXEC` / `ABI_RIGHTS_MASK` / `ABI_PAGE_SIZE` |
| `enum ipc_service_tag` | `service.h` | 服务间 IPC 的 tag 常量（`TAG_SM_*` / `TAG_PM_*` / `TAG_FS_*` / `TAG_CONSOLE_*` / `TAG_VM_*`） |
| `enum service_sid_user` | `service.h` | 主体身份 SID（1..9）；**内核与用户态的唯一权威定义** |

## 与其他模块的关联

| 对方模块 | 关系 |
| --- | --- |
| `kernel/syscall` | 用 `table[abi_number]` 建分发表；`shape_ok` 用 `abi_tail_zero` / `abi_arg_u32`；`finish` 用 `abi_result` 写回帧 |
| `kernel/syscall/user_backend` | 每个 `ABI_*` 的实现；把 `CAP_E_*`/`MEM_*`/`IPC_*` 转成 `enum abi_status` |
| `userland/libs/runtime.c` | 用户侧包装：`user_ipc_call` 等用 `abi_message_args`；`abi_decode_result` 解包 |
| `userland/libs/service_proto.h` | **只引用** `service.h`，不再维护副本（A22 修掉了两份独立定义漂移的风险） |
| `userland/servers/*/user/*.c` | 用 `TAG_*` 与 `SVC_*` 常量编写服务协议 |
| `kernel/boot/service_boot.c` | 用 `SERVICE_SID_*` 决定 cap 装到哪个主体 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 裸寄存器约定 | 只有文档：`a7` 号、`a0..a5` 参数、`a0` 返回状态 | 单模块 syscall demo 先跑通闭环 |
| 2. 共享编解码内联函数 | `abi_meta` / `abi_result` / `abi_decode_*` 放进头文件 | 内核与用户态各写一份位运算必然会漂移 |
| 3. 形状校验原语 | `abi_tail_zero` / `abi_arg_u32` / `abi_page_aligned` / `abi_rights_valid` | 内核不能相信用户寄存器高位；这些检查原先分散在 handler 里 |
| 4. 消息 meta 复用 `a1` | `meta[31:0]=tag`、`meta[63:32]=reply` | 一条 `ecall` 只有 6 个参数寄存器，tag 与 reply 必须打包 |
| 5. 状态码统一为负值 | `enum abi_status`；`abi_decode_status` 做符号扩展 | 正数曾被误当成功；统一为「0 成功、负值错误」后调用方判断简单 |
| 6. 服务协议常量入库 | `service.h` 定义 tag 与 SID，内核与用户态共用 | 新增 tag 时漏改一份会导致 U-mode 发出的消息内核不认识（A22） |
| 7. `ABI_TIMED_OUT`（A16） | 新增 -11，供 pager 缺页超时使用 | 超时需要与 `ABI_BUSY`（短暂重试）、`ABI_CLOSED`（对端消失）区分 |
| 8. `ABI_BLOCK_READ`/`INFO`（A1） | 新增 27/28；删除 `ABI_VFS_READ_AT`（13 留空洞） | ext4/vfs 迁到 U-mode 后，内核只暴露原始块访问，不再解释路径 |

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

`ep`、`as`、`frame` 是调用者能力空间中的不透明句柄。映射虚拟地址 `va` 按 4 KiB 对齐，`rights` 的低三位依次表示读、写、执行。可写映射同时要求读权限，且不得同时可执行（W^X）。 [memory 模型](../../kernel/memory/README.md)

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
| -10 | `NOT_FOUND` | 路径/对象不存在 |

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
## M3/M6/M7 扩展调用号与权限

| nr | 名称 | 权限 |
| --- | --- | --- |
| 9 | `ABI_CAP_MINT_TO` | 仅 SM；跨 SID 需源 cap 带 `GRANT` |
| 10 | `ABI_PM_SPAWN` | 仅 PM；`a2` 保留；成功时 `words[0]=展示 pid`，`words[1]=thread object id`，`words[2]=PM 侧 AS 源 cap` |
| 11 | `ABI_CAP_DELETE` | 删除调用者自己 SID 里的 cap |
| 13 | ~~`ABI_VFS_READ_AT`~~ | **已删除（A1）**：ext4/vfs 迁到 U-mode，内核不再解释路径 |
| 14 | `ABI_CONSOLE_READ` | 仅 console 服务（非阻塞，`ABI_BUSY`） |
| 15 | `ABI_CONSOLE_WRITE` | 仅 console 服务 |
| 16 | `ABI_POWEROFF` | 仅 shell |
| 17 | `ABI_PID_SELF` | 任意 U-mode 线程（返回 thread object id） |
| 19 | `ABI_VM_MAP` | 仅 VM；须持 pager 的一次性 map 票 |
| 20 | `ABI_PAGER_DONE` | 仅 VM；唤醒缺页线程 |
| 21 | `ABI_AS_SELF` | 任意 U-mode 线程（返回自己 AS 的 root pfn） |
| 22 | `ABI_PAGER_FAIL` | 仅 VM；拒绝缺页并结束 fault 线程 |
| 23 | `ABI_PAGER_READY` | 仅 VM；宣告 pager 可用 |
| 24 | `ABI_REPLY_CALLER_SID` | 仅 SM 与 VM；`a0=reply_id` → 真实发起者 `cap_sid`（SM 做名称服务鉴权，VM 校验 `TAG_VM_FORGET` 的发送者） |
| 25 | `ABI_VM_GRANT_AS` | 仅 PM；`a0=thread_id,a1=pm_as_cap`（0=内核直装 VM）；成功时 `words[0]=目标 AS 的 object id` |
| 26 | `ABI_PM_DISCARD` | 仅 PM；`a0=thread_id,a1=spawn image`；撤回 spawn 产物（含已 RUNNING 的延迟退出） |
| 27 | `ABI_BLOCK_READ` | 仅 FS；`a0=lba,a1=udst,a2=blocks` 读原始逻辑块（A1：策略层在 U-mode） |
| 28 | `ABI_BLOCK_INFO` | 仅 FS；`words[0]`=容量 `words[1]`=块大小 |

12、13、18 是空洞；对空洞发 ecall 返回 `ABI_BAD_NR`，不会调用空 handler。
（13 曾是 `ABI_VFS_READ_AT`，A1 把 ext4/vfs 迁到 U-mode 后删除。）
`ABI_NOT_FOUND = -10`，`ABI_TIMED_OUT = -11`。**没有共享 IPC 页**：文件内容/路径/console
输出都按 16 B 或 24 B 的 IPC 分块传输。
