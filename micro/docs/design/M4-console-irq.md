# M4 控制台与 PLIC（设计备忘）

## 裁决

- **UART 驱动放在 `kernel/console/`**：panic 与外部中断 top-half 必须不依赖用户服务；
  M7 的控制台服务可后续通过 capability 复用同一 MMIO。
- **PLIC 在 `kernel/arch/riscv/plic.c`**：实现 `irq_port`，`irq.c` 保持零 MMIO。

## MMIO 映射

`kernel_enter_resident` 在 `kernel_as` 上恒等映射：

| 区域 | 范围 | 权限 |
| --- | --- | --- |
| UART0 | `0x10000000` 4KiB | 内核 R/W，无 U |
| PLIC | `0x0c000000` 4MiB | 同上 |

外部中断处理前 `kernel_irq_enter_devices()` 切换到 `kernel_as`，返回用户线程前恢复。

## 验收命令

```bash
timeout 8s make -C micro run
MICRO_FS_IMAGE=$PWD/build/micro-root-m2.img python3 tools/console_irq_test.py
```

`console_irq_test.py` 等到 `[console] switched from SBI putchar` 后才向真实 QEMU stdin 写入 `abc\r`，并要求 IRQ10、完整行和后续心跳中的 `ext_irqs>0`。早发管道实测会丢首字节，该失败保留在 M4 日志，不作为通过证据。