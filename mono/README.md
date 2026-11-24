# operating-systems-lab

一个最小可运行的多架构教学操作系统内核：同一套通用代码同时支持
**RISC-V 64（QEMU virt）** 和 **ARM Cortex-A9 / Zynq-7010（Z7-Lite 开发板）**。
按 [`docs/goals/`](docs/goals/README.md) 的 G0–G14 实现，教材对照以王道第 1–5 章、
汤小丹《计算机操作系统》第 1–9 章为主。

## 当前状态

已实现（两架构共用，另有说明除外）：

- 启动入口、启动栈、BSS 清零、链接脚本
- 轮询式串口控制台；10 ms 时钟中断
- 物理内存位图分配器（4 KiB 页）
- RISC-V Sv39 页表：内核恒等映射 + 每进程用户页表
- ARM 短描述符 MMU：DDR/UART/GIC 用 1 MiB 段映射；支持 4 KiB L2 小页（`vm_selftest` 已验证），用户区仍用 1 MiB 段
- 系统调用 ABI：`write/read/exit/getpid/yield/fork/exec/sbrk/open/close/mkdir/ps`
- 用户态 flat 镜像：`hello`、`forktest`、`faulttest`、`init`、`sh`
- ARM 用户态 `hello`、`fork`、`exec` 与 shell 经 SVC 跑通（1 MiB 段映射）
- PCB、内核线程、时间片轮转调度（RISC-V 时钟抢占；ARM 安全点让出）
- 信号量、互斥量、生产者-消费者（1000 次）、银行家算法自检
- 请求分页、按需零页、FIFO 驱逐、RAM 交换区（RISC-V）
- 块缓冲层（16 个 4 KiB 缓冲区）、1 MiB RAM 盘
- 简易文件系统：superblock、inode、位图、根目录、`/motd`
- 设备开关表：`/dev/null`、`/dev/zero`
- 磁盘 SCAN 调度自检
- init + 最小 shell：`help`、`ps`、`cat`、`echo`、`fork_test`、`mkdir`、`exit`
- Z7-Lite SD/U-Boot 启动文档与部署脚本（默认 dry-run）

尚未实现或未验证：

- **ARM 缺页**：ARM 用户态已支持 `hello/fork/exec/shell`，但未接入按需分页/缺页处理
- **ARM 完整抢占**：定时器只置 `need_resched`，在长循环的安全点由 `sched_check()` 让出；IRQ 不做完整 trap frame 切换
- ARM D-cache 仍关闭，页表一致性留待后续
- COW、间接块、文件删除/截断、权限、ext4、日志
- SMP、网络协议栈、设备树解析、完整 POSIX
- **当前多 Goal 内核尚未重新在 Z7-Lite 实板验证**；实板证据是 G1 之前的基线内核

## 环境要求

构建使用 **clang** 和 LLD，不需要 GCC 交叉工具链。

| 工具 | 用途 |
|---|---|
| `clang` | 编译 RISC-V / ARM 目标 |
| `ld.lld` 或 `rust-lld` | 链接；Makefile 自动查找 |
| `qemu-system-riscv64` | 运行 RISC-V 版本 |
| `qemu-system-arm` | 运行 ARM/Zynq 版本 |
| `llvm-objcopy` | 生成 ARM `kernel.bin` |
| `python3` | 可选：`tools/mkfs.py` 生成离线 RAM 盘镜像（内核启动时在内存中格式化，不读取该文件） |

Ubuntu/Debian：

```sh
sudo apt install qemu-system-misc qemu-system-arm lld llvm
```

## 构建

```sh
make                 # RISC-V，产物 build/riscv/kernel.elf
make ARCH=arm        # ARM/Zynq，产物 build/arm/kernel.elf
make ARCH=arm bin    # 额外生成 build/arm/kernel.bin（U-Boot 可加载）
make fs-image        # 可选：生成离线镜像 build/ramdisk.img（内核不挂载）
make clean
```

`tools/build.sh [riscv|arm|clean]` 是 Makefile 的薄封装。

`make fs-image` 生成的 `build/ramdisk.img` 是供检查的离线布局镜像；内核启动时在
内存中格式化自己的 RAM 盘（内容与 `userland/motd.txt` 一致），不读取该文件。

## 运行

RISC-V：

```sh
make run
# qemu-system-riscv64 -machine virt -m 128M -nographic -bios default \
#     -kernel build/riscv/kernel.elf
```

ARM/Zynq：

```sh
make ARCH=arm run
# qemu-system-arm -M xilinx-zynq-a9 -m 512M -nographic \
#     -kernel build/arm/kernel.elf
```

在 QEMU 中按 `Ctrl-A` 然后 `X` 退出。脚本里可限时：

```sh
timeout 20s make run
timeout 15s make ARCH=arm run
```

## 启动路径

### RISC-V

```text
QEMU virt / OpenSBI → _start 0x80200000 → kmain
  → pmm → Sv39 → bio/fs → proc/scheduler → init → sh
```

### ARM / Zynq-7010

```text
上电 → BootROM → FSBL（原厂 BOOT.BIN）→ U-Boot
  → kernel.bin 0x00100000 → _start 0x00100020 → kmain
```

U-Boot 片段（详见 [docs/Z7-Lite-kernel-boot-snippet.md](docs/Z7-Lite-kernel-boot-snippet.md)）：

```text
fatload mmc 0:1 0x00100000 kernel.bin
dcache flush
icache flush
go 0x00100020
```

### 用户地址空间

```text
0x40000000  用户代码 / 数据（flat 镜像，两架构共用）
0x40080000  堆起点（sbrk，按需分页）
0x400ff000  用户栈页
0x40100000  栈顶
```

每个 RISC-V 用户进程有独立 Sv39 页表，内核 RAM/UART 映射在所有页表中保留。

## 目录结构

```text
Makefile                     双架构构建 + 用户态镜像 + 离线 fs-image
kernel/
  boot/boot.c                启动编排、自检、首个用户程序
  lib/                       字符串、kprintf
  proc/                      PCB、线程、调度、用户镜像/exec/fork
  sync/                      信号量、互斥量、银行家算法
  mm/                        VM 公共 API、缺页/驱逐/交换
  fs/                        块缓冲、RAM 盘、文件系统、磁盘调度
  dev/                       设备开关表（console/null/zero）
  tests/                     调度与生产者-消费者演示
  arch/riscv/                Sv39、trap、switch、UART、SBI、linker
  arch/arm/                  短描述符 MMU、vectors、switch、UART、GIC、linker
shared/abi/syscall.h         跨态系统调用 ABI
userland/                    hello/forktest/faulttest/init/sh + 链接脚本
tools/                       mkfs.py、deploy-kernel-sd.sh、构建/调试脚本
docs/goals/                  G0–G14 Goal 正文
docs/design/                 每个 Goal 的教材对照与实现取舍
docs/roadmap.md              章节 ↔ 目录 ↔ Goal ↔ 验收命令
```

## 本轮验收输出（2026-11-19）

### RISC-V（QEMU 8.2.2 + OpenSBI）

```text
[boot] operating-systems-lab: minimal kernel
[vm] Sv39 enabled, root=0x8039c000
[vm] selftest map/unmap ok
[bio] pattern rw ok; hit=1 miss=1
[fs] formatted: 256 blocks, 64 inodes, root ready
[fs] /motd: Welcome to operating-systems-lab
This is a teaching OS on RISC-V and ARM Zynq-7010.
[dev] null/zero ok (devices=3)
[disk] SCAN from 53 (up): 65 67 98 122 124 183 37 14
[user] hello pid=2 code=209 bytes at 0x40000000
hello from user
[banker] safe seq: P1 P3 P0 P2 P4
[sync] prodcons ok count=1000
faulttest: demand paging and swap ok
[vm] faults=192 evictions=144
[init] root filesystem ready
[demo] CPU utilization: single-program ~40%, multiprogram ~80%
sh>
```

### ARM/Zynq（QEMU 8.2.2）

```text
[vm] ARM short-descriptor MMU enabled, L1=0x25c000
[vm] selftest map/unmap ok (ARM 4KiB)
[bio] pattern rw ok; hit=1 miss=1
[fs] formatted: 256 blocks, 64 inodes, root ready
[fs] /motd: Welcome to operating-systems-lab
This is a teaching OS on RISC-V and ARM Zynq-7010.
[dev] null/zero ok (devices=3)
[disk] SCAN from 53 (up): 65 67 98 122 124 183 37 14
[proc] 0 RUN kmain
[proc] 1 READY idle
[user] ARM hello pid=2 code=345 bytes at 0x40000000
hello from user
[user] ARM forktest pid=3 ...
parent: fork returned child pid
[user] ARM init pid=4 ...
[init] root filesystem ready
[init] exec /sh
sh> ...
[sched] pid=5 tick=0
```

ARM 已跑通 `hello/fork/exec/shell`（`cat /motd`、`help`、`fork_test` 均可）。
ARM 的两个 worker 通过 `sched_check()` 在安全点交替，不是 IRQ 直接抢占；用户区仍是 1 MiB 段映射。

## 教材与 Goal

- Goal 总览：[docs/goals/README.md](docs/goals/README.md)
- 全局合同：[docs/goals/GLOBAL-CONTRACT.md](docs/goals/GLOBAL-CONTRACT.md)
- 章节路线图：[docs/roadmap.md](docs/roadmap.md)
- 每个 Goal 的设计取舍：[docs/design/](docs/design/)
- 实机记录：[docs/Z7-Lite-SD卡经U-Boot启动内核实录.md](docs/Z7-Lite-SD卡经U-Boot启动内核实录.md)、[docs/Z7-Lite-JTAG连接与调试实录.md](docs/Z7-Lite-JTAG连接与调试实录.md)

## 下一阶段建议

1. 补齐 ARM 数据 abort 缺页处理（按需分页/交换）。
2. 开启 ARM D-cache，补齐页表与用户镜像的一致性维护。
3. 重新在 Z7-Lite 上用 U-Boot 验证当前内核，记录新的实板日志。
4. COW fork、间接块、文件删除、权限位。
5. 设备树解析、SMP、网络（超出当前教学范围，逐项开新 Goal）。
