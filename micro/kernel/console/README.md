# console：UART0 行输入与内核输出

UART0（16550）MMIO 输出 + PLIC 外部中断行输入。resident 下输出优先
`console_puts()`（直接写 MMIO）；SBI 仅用于早期 boot 与 panic。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct irq_sink console_sink` | `console.c` | 绑到 `UART0_IRQ`(10) 的中断接收者，`notify = console_uart_notify` |
| `line_buf[]` / `line_len` | `console.c` | 正在拼装的一行（`CONSOLE_LINE_MAX = 80`）；收到 `\r`/`\n` 时整行入队 |
| `line_queue[8][80]` + `line_queue_len[]` | `console.c` | 已完成的输入行环形队列；容量 `CONSOLE_QUEUE_LINES = 8` |
| `line_queue_head` / `line_queue_count` | `console.c` | 队列游标；满时**丢弃最旧**一行 |
| `line_waitq` | `console.c` | 阻塞版读取（`console_readline`）的等待队列 |
| `ext_irqs` / `timer_ticks` | `console.c` | 观测计数：外部中断次数、timer tick 次数（heartbeat 日志用） |
| `saved_divisor` / `mmio_on` | `console.c` | UART 分频值与「是否已切到 MMIO」标志 |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `interrupt` + `arch/riscv/plic` | 中断来源 | `console_bind_uart_irq()` → `irq_bind(UART0_IRQ, &console_sink)`；`console_uart_notify` 末尾 `irq_ack` |
| `sched` | 队列互斥与唤醒 | `flush_line` / `console_read_line` 全程持 `sched_lock()`；`flush_line` 对 `line_waitq` 做 `thread_wake_locked` |
| `syscall/user_backend` | `ABI_CONSOLE_*` 实现 | `ABI_CONSOLE_READ`（仅 console SID，非阻塞，空队列 `ABI_BUSY`）/ `ABI_CONSOLE_WRITE`（仅 console SID，`as_copyin` 后逐字节输出） |
| `shared/demo.c`（trap glue） | 早期输出 | `console_mmio_active()` 为假时走 SBI putchar；boot 完成后切 MMIO |
| `boot/kernel_init` | 初始化 | `console_mmio_init(divisor)` → `console_mmio_enable(true)` → `console_bind_uart_irq()` |
| `userland/servers/console` | U-mode 服务 | 服务自己不碰 UART；它把 shell 的 `TAG_CONSOLE_WRITE/READ` 转成 `ABI_CONSOLE_*` |
| `kernel/pager` 等 | 内核日志 | `demo_log` → `console_puts`；`reject_and_exit` 也直接用它 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 只有 SBI 输出 | `sbi_putchar` 逐字符 | boot 早期还没有 MMIO 映射，SBI 是唯一可用通道 |
| 2. MMIO 输出 | `console_mmio_init(divisor)` 设 8N1 + 分频；`mmio_putc` 等 `LSR_THRE` 再写 | SBI 调用开销大，且无法在 U-mode 代执行路径里使用 |
| 3. 中断驱动输入 | `console_uart_notify` 逐字节读 RBR，`\r`/`\n` 时 `flush_line` | 轮询会浪费 CPU；demo.sh 需要真实 stdin |
| 4. 有界行队列 | 8 行 × 80 字节环形队列，满时丢最旧 | 用户可能一次粘贴多行；队列必须有界且不阻塞 IRQ |
| 5. 两种读取接口 | `console_read_line`（非阻塞，供 `ABI_CONSOLE_READ`）+ `console_readline`（阻塞，供内核线程） | U-mode console 服务需要非阻塞轮询，内核 demo 需要阻塞语义 |
| 6. 队列加锁（A8） | `console_read_line` 全程持 `sched_lock` | 原实现隐含依赖「调用点恰好 SIE=0」，与 UART IRQ 的 `flush_line` 存在竞争窗口 |
| 7. 回显与退格 | `echo_backspace` / 逐字符回显 | 交互可用性；demo.sh 的断言也依赖回显出的命令行 |
| 8. 计数器与 heartbeat | `ext_irqs` / `timer_ticks`，由 `console_note_timer_tick` 累加 | M4 验收需要「IRQ 真的在增长」的可观测证据 |

## 内核 ABI

| 调用 | 权限 | 语义 |
| --- | --- | --- |
| `ABI_CONSOLE_READ` | 仅 `SERVICE_SID_CONSOLE` | 非阻塞；空队列 `ABI_BUSY`，服务以 `ABI_YIELD` 轮询 |
| `ABI_CONSOLE_WRITE` | 仅 `SERVICE_SID_CONSOLE` | 从用户缓冲 `as_copyin` 后 `console_puts`，单次上限 128 字节 |

U-mode console 服务（`userland/servers/console/user`）负责 SM 注册与存活；
shell 不直接调用内核，它把 `TAG_CONSOLE_READ/WRITE` 发给 console 服务。

## 已知边界

- 行队列只有 8 行；满时丢最旧，不阻塞、不报错。快速粘贴大量输入会丢行。
- 单行上限 80 字节（超出部分被丢弃）。
- 输出无缓冲、无背压：`cat` 一个大文件时 shell 与 console 服务的 24 B
  分块 IPC 是吞吐瓶颈（`ABI_WORDS=4` 的固有代价）。
- 只服务 UART0 一条线；virtio-blk 在 resident 下走轮询而非中断。
