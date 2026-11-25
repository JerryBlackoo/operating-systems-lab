# [M4] 真实控制台：UART0 MMIO + PLIC 中断

**依赖**：M1（可与 M2 并行）  
**建议 Agent**：A4  
**波次**：3

## 背景

现在整个系统的输出只有一个通道：`sbi_putchar()`（`kernel/arch/riscv/sbi.c`）——每次打印一个字符就是一次 `ecall` 陷入 M 态。而且：

- **没有任何 UART 驱动**。`UART0_BASE` 只在 `platform.h` 定义为常量，`kernel/memory/README.md` 甚至写"内核需要在启动时单独映射 UART"，但没人做过。
- **PLIC 只有契约没有实现**。`kernel/interrupt/irq.h` 定义了 `struct irq_port` 的 6 个操作（`claim/complete/mask/unmask/intr_save/intr_restore`），`irq.c` 里全文只有一句注释提到 PLIC，没有任何 MMIO 读写。`kernel/interrupt/README.md` 第 131 行给的 `plic_claim()` 是**示意代码**，不是本仓库实现。
- **用户无法输入**。`demo_enter_user` 建立的 trap 只处理 timer 与 ecall，键盘输入无处可去。

M7 的 shell 没有这个 Goal 就是空中楼阁。

## 教材对照

| 王道 | 汤小丹 |
| --- | --- |
| 5.1 I/O 管理概述 | 第 9 章 设备管理 |
| 5.2 I/O 软件层次结构（中断处理程序） | 9.2 中断处理 |
| 5.2 设备驱动程序接口 | 9.3 设备驱动程序 |

## 目标

1. **UART0 驱动**（新目录 `kernel/console/` 或 `userland/servers/drivers/console/`，**位置在 design 文档里裁决并说明理由**）：
   - 初始化：8N1、波特率按 QEMU virt 的 3.6864 MHz 时钟计算 divisor；
   - 发送：轮询 THR 就绪位（`LSR.THRE`），提供 `console_putc/puts`；
   - 接收：`IER.RXIE` 打开，收到字节进环形缓冲；
   - 不用 SBI `putchar` 做常规输出。**SBI 只保留给 panic 路径**（因为 UART 未映射时 panic 仍要能打印）。
2. **映射 UART 进内核 AS**：`0x10000000` 必须在内核页表里（`as_map_kernel_range` 或专用设备映射，权限 `R|W`，**不带 U**）。验收要能证明：早期 boot 用 SBI，进入 `kernel_init` 后切到 MMIO，且切换点在日志里可见。
3. **PLIC 真实实现**：给 `struct irq_port` 写一个 RISC-V 版本（例如 `kernel/arch/riscv/plic.c`）：
   - `claim`：读 `PLIC_BASE + 0x201004`（hart0 S 上下文，`kernel/interrupt/README.md` 第 1 节给了实测地址）；
   - `complete`：写 `PLIC_BASE + 0x201004`；
   - `mask/unmask`：读写 `PLIC_BASE + 0x2080 + 4*(id/32)` 的 enable 位；
   - **threshold** 必须初始化为 0，否则 claim 永远返回 0；
   - `intr_save/restore` 走 `sstatus.SIE`。
4. **接上 trap**：`shared/demo.c` 的 `trap_handler` 目前只处理 `IRQ_S_TIMER`。新增 `IRQ_S_EXT`（`scause` 9）分支：
   ```text
   scause == 9 → irq_port.claim() → 分派到 console 的 sink → irq_port.complete()
   ```
   必须处理"claim 返回 `IRQ_NONE`"的情况（`irq.c:182` 已注明"无主事件也必须 complete"）。
5. **行缓冲与回显**：控制台服务维护行缓冲，收到 `\r` 时把整行交给上层（M7 的 shell），并回显 `\r\n`；`\b`/`0x7f` 支持退格。

## 交付

- `kernel/console/{console.c,console.h}` 或 `userland/servers/drivers/console/`（按 design 裁决）
- `kernel/arch/riscv/plic.c`（+ 必要时 `plic.h`）
- `shared/demo.c`（`IRQ_S_EXT` 分支）
- `kernel/boot/kernel_init.c`（UART 映射与切换）
- `kernel/interrupt/README.md` 的「平台端口」节补上本实现的位置
- `docs/design/M4-console-irq.md`、`docs/evidence/M4-<日期>.log`

## 验收

```bash
timeout 15s make -C micro run; echo "EXIT=$?"
# 期望：
#   [console] uart0 mmio ready (divisor=<n>)
#   [console] switched from SBI putchar
# 且 EXIT=124
```

中断路径（必须有判别力，不能只看"打印正常"）：

```bash
# 用 QEMU 的标准输入喂字符（-nographic 下 stdin 直连 UART）
printf 'abc\r' | timeout 15s make -C micro run
# 期望日志出现：
#   [console] irq: line=10
#   [console] line="abc"
```

> 若 `-nographic` 的 stdin 行为与预期不符，改用 `-serial mon:stdio` 或显式 `-chardev` 并在 design 文档记录实际可用写法。**不许把"能打印"当成"中断通了"。**

PLIC 本身的判别证据：

```bash
# 在 irq 统计里区分 timer 与外部中断
# 期望 idle 心跳里能看到 ext_irqs 计数随按键增长，而 timer_ticks 独立增长
```

## 非目标

- 不做完整 tty（termios、流控、多路复用）
- 不做 UART DMA
- 不做 virtio-console（仍用 16550 兼容的 UART0）
- 不把 console 做成必需服务：内核 panic 路径不依赖它

## 已知风险

- **`-nographic` 把 stdin 接到 UART，但只在收到换行/特定序列时透传**。开工先用 `qemu-system-riscv64 ... -nographic` 手工敲字符验证，再写自动化脚本，别反过来。
- **PLIC 的 S 上下文 enable 地址容易抄错**。`kernel/interrupt/README.md` 给的是 `0x0c002080`（enable）与 `0x0c201004`（claim/complete），必须用 `dumpdtb` 复核一次再写常量，并在 design 文档记录复核命令。
- **UART 未映射时的 panic**：如果 `kernel_init` 在映射 UART 之前就 panic，走 SBI 是对的。要保证 panic 路径**永远**走 SBI，不依赖当前用哪种 console。

## 纪律

必读：王道 5.1、5.2；汤小丹第 9 章。  
冲突时：`kernel/interrupt/irq.h` 的 `irq_port` 六操作为唯一接缝，不许在 `irq.c` 里直接写 PLIC 寄存器。  
完成后：只提交本 Goal 范围 diff，附 `docs/evidence/M4-<日期>.log`（含 `irq: line=10` 与 `line="abc"`）。
