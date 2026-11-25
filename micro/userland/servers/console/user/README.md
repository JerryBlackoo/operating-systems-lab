# console 服务（U-mode）

`userland/servers/console/user/console_main.c` 是 M7 的真实 console 服务。它
**不碰 UART**：所有实际 I/O 都经内核的 `ABI_CONSOLE_READ/WRITE`（仅
`SERVICE_SID_CONSOLE` 可调）。它的职责是把 shell 的「写一段」「取一行」翻译成
内核调用，并管理跨块读取的续传状态。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `pending_line[128]` / `pending_len` | `console_main.c` | 当前正在分块交给客户端的**一整行**；`TAG_CONSOLE_READ` 填它，`TAG_CONSOLE_MORE` 按 offset 续传 |
| `pack24` / `unpack24` | `console_main.c` | 24 字节内联字的打包/解包（与 fs 服务同一套约定） |
| `reply_msg` | `console_main.c` | 统一的回复构造 |
| `do_write` / `do_read` / `do_more` | `console_main.c` | 三种请求的处理函数 |
| `fill_pending_line` | `console_main.c` | 调 `user_console_read` 拿一行；`ABI_BUSY` 时 `ABI_YIELD` 自旋 |
| `send_chunk` | `console_main.c` | 从 `pending_line` 的 `off` 处发一块（≤ 24 B） |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `kernel/console` + `syscall/user_backend` | 唯一 I/O 通道 | `ABI_CONSOLE_READ`（非阻塞，空队列 `ABI_BUSY`）/ `ABI_CONSOLE_WRITE`（`as_copyin` 后逐字节输出） |
| `kernel/interrupt` + `PLIC` | 输入来源（间接） | UART0 IRQ 10 → `console_uart_notify` → 行队列 → `ABI_CONSOLE_READ` |
| `sm` | 服务注册 | 启动先 `TAG_SM_REGISTER`（名字 `"console"`） |
| `init` | 启动握手 | 注册后回执 `TAG_READY`；init 等它和 fs 都 ready 才 spawn shell（A31） |
| `shell` | 唯一客户端 | `TAG_CONSOLE_WRITE` / `TAG_CONSOLE_READ` / `TAG_CONSOLE_MORE`，24 B 内联字 |
| `pm` | 进程可见性 | 服务存活使 PM 的进程表能看到 `console`（`ps` 的 total 含它） |

## 启动参数

| 参数 | cap |
| --- | --- |
| `a0` | ready send cap |
| `a1` | receipt `0x2002` |
| `a2` | console endpoint recv cap（slot 2） |
| `a3` | 本服务 `sid` |
| `a4` | SM register send cap（slot 3） |

与 fs 服务同一套槽位布局（`SLOT_CHILD_READY` / `SLOT_CHILD_RECV` /
`SLOT_CHILD_SM`），因此 `service_teardown_child_caps` 能统一拆除。

## 消息协议

| tag | words | 含义 |
| --- | --- | --- |
| `TAG_CONSOLE_WRITE` (0x601) | `[0]=n, [1..3]=24 B 数据` | 写一段；`n ≤ 24` |
| `TAG_CONSOLE_WRITE_OK` (0x602) | `[0]=n` | 已写出 |
| `TAG_CONSOLE_READ` (0x603) | — | 取一整行的**第一块**；空队列回 `TAG_CONSOLE_ERROR(ABI_BUSY)` |
| `TAG_CONSOLE_READ_OK` (0x604) | `[0]=总长, [1..3]=24 B` | 行总长与本块数据 |
| `TAG_CONSOLE_MORE` (0x605) | `[0]=offset` | 续传这一行的下一块 |
| `TAG_CONSOLE_MORE_OK` (0x606) | `[0]=本块长, [1..3]=24 B` | 续传数据 |
| `TAG_CONSOLE_ERROR` (0x607) | `[0]=负数 ABI 状态` | 出错 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. shell 直接调内核 | M7 早期 shell 自己发 `ABI_CONSOLE_READ/WRITE` | 少一跳就少一份状态；但内核里就必须有「只有 shell 能用」的特权判断 |
| 2. 独立服务（当前） | shell 只持有 console endpoint cap，内核 `ABI_CONSOLE_*` 只允许 `SERVICE_SID_CONSOLE` | 把「谁可以碰 UART」收敛到一个主体；shell 变成纯客户端 |
| 3. `MORE` 续传 | 一行可能超过 24 B，用 `TAG_CONSOLE_MORE` 按 offset 续传 | `ABI_WORDS = 4` 让单条消息只有 24 B 载荷 |
| 4. `pending_line` 状态 | 读请求到达时把整行拷进服务内缓冲，之后按 offset 切块 | 内核的行队列是消费型（读一次就出队）；不缓存就没法分块重发 |
| 5. `ABI_BUSY` 自旋 | `fill_pending_line` 在空队列时 `ABI_YIELD` 再试 | 内核侧是非阻塞接口；服务自己承担等待 |
| 6. 服务存活即进度 | 即使没有新命令也要保持在 `recv` 上 | PM 进程表与 `ps` 的 total 依赖它活着；也保留未来 tty/多路复用的扩展点 |

## 已知边界

- `pending_line` 只有 128 B；超过一行的输入被内核侧截断到
  `CONSOLE_LINE_MAX = 80`。
- 只有单个客户端（shell）。没有 tty 概念、没有多路复用、没有输入历史。
- 输出无背压：`TAG_CONSOLE_WRITE` 逐 24 B 同步等 ACK，`cat` 大文件时这条
  路径本身就是吞吐瓶颈。
- 服务不做权限检查——内核已经保证只有 `SERVICE_SID_CONSOLE` 能调
  `ABI_CONSOLE_*`，而谁能给它发 `TAG_CONSOLE_*` 由 cap 分发决定。
