# bootloader/

SeaBIOS、MBR、PBR 和内核之间的交接可以在本仓库的实验中逐段观察。[x86 BIOS 实验](../../ex/x86-bios/README.md)

## RISC-V 启动

RISC-V 路径借助 QEMU 的 `-bios default` 和随 QEMU 安装的 OpenSBI 固件完成加载与硬件交接。

- QEMU 的 `-kernel` 选项负责把 `build/riscv/kernel.elf` 加载到它的链接地址 `0x80200000`。
- OpenSBI `fw_dynamic` 在 M-mode 完成最早期初始化（PMP、时钟、控制台、设备树准备等），
  然后切换到 S-mode 并跳转到内核入口。
- 内核入口 `_start` 位于 `kernel/arch/riscv/boot.S`。进入该入口时，引导阶段已经结束。

启动链路：

```text
QEMU virt 复位
  -> OpenSBI fw_dynamic @ 0x80000000（M-mode）
  -> S-mode 跳转 @ 0x80200000
  -> kernel _start（boot.S）
       -> 设置 sp = __stack_top
       -> 清零 .bss
       -> kmain
```

对应命令：

```sh
qemu-system-riscv64 -machine virt -m 128M -nographic -bios default -kernel build/riscv/kernel.elf
```

## 地址约定

以下地址对应当前 `QEMU virt + 128 MiB` 配置，并与 `kernel/arch/riscv/linker.ld` 保持一致：

| 地址 | 内容 |
| --- | --- |
| `0x80000000` | OpenSBI 固件基址 |
| `0x80000000`–`0x80200000` | 保留给 OpenSBI（固件、工作区等，具体布局由 OpenSBI 决定），内核不使用 |
| `0x80200000` | 内核入口 `_start` / `.text` 起始，由链接脚本固定 |
| `0x10000000` | NS16550A UART（MMIO） |
| `0x87e00000` | 本次启动中设备树（DTB）的物理地址，来自 OpenSBI 日志的 `Domain0 Next Arg1`；内核尚未解析，不能硬编码依赖 |
| `0x88000000` | 128 MiB RAM 结束地址 |

当前没有早期页表：`satp = 0`，运行在 bare mode，代码和数据都使用物理地址。
内核只使用自身镜像内的 `.bss` 作为 16 KiB 启动栈，还没有堆、物理内存分配器或页表。

## 入口寄存器约定（OpenSBI → 内核）

按 RISC-V SBI / OpenSBI 的交接约定：

- 特权级：S-mode（OpenSBI 启动日志中的 `Domain0 Next Mode : S-mode`）。
- `a0`：当前 hart ID。本配置只启动 hart 0。
- `a1`：设备树（DTB）的物理地址。本次运行日志显示 `Domain0 Next Arg1 : 0x0000000087e00000`。
- `satp`：0，未开启分页。
- 中断：不要让内核依赖引导阶段的中断状态；`boot.S` 会清零 `sie`/`sip`。
- `sp`：进入 `_start` 时不可依赖，`boot.S` 会立即设置为 `__stack_top`。

注意：当前内核入口没有保存 `a0`/`a1`。进入 C 函数 `kmain` 后不能依赖这两个寄存器，
后续引入内存范围和设备树解析时，需要在 `_start` 中先保存成明确的启动参数结构。

## 验收

已在本机 QEMU 8.2.2 + OpenSBI v1.3 上验证（2026-11-18）：

```sh
make
timeout 6s make run
```

日志中可以看到：

```text
Domain0 Next Address      : 0x0000000080200000
Domain0 Next Mode         : S-mode
...
[boot] operating-systems-lab: minimal RISC-V 64 kernel
[boot] boot path: QEMU virt -> OpenSBI -> S-mode entry
...
[boot] entering wfi loop (Ctrl-A X to exit QEMU)
```

即 QEMU 能稳定进入内核入口 `_start` 并完成控制台输出；内核随后停在 `wfi` 循环，
不会自行退出，交互时用 `Ctrl-A X` 结束 QEMU。

## 后续要补的内容

1. 决定启动策略：继续借助 OpenSBI，还是实现自研 bootloader / M-mode 入口。
2. 若继续借助 OpenSBI：在 `_start` 保存 `a0`（hart ID）和 `a1`（DTB 地址），
   解析设备树中的内存范围、保留内存和串口信息，再传给 `kmain`。
3. 在需要时建立早期页表（例如 Sv39 内核直接映射）；当前版本明确不做。
4. 若改为自研 bootloader：需要定义自己的加载方式、PMP/页表配置、内核镜像格式和
   启动参数结构，并同步更新 `linker.ld` 中的地址约定。
5. 把上述约定与根目录 `README.md`、`kernel/arch/riscv/linker.ld` 保持一致；
   在这些内容真正实现前，不在此目录添加空壳代码。
