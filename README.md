# operating-systems-lab

一个最小可运行的 RISC-V 64 操作系统内核，目标平台是 QEMU 的 `virt` 机器。
当前版本完成“能启动、能看见输出、能接住陷阱和时钟中断、能分配物理页”的内核底座，
**没有**虚拟内存、进程管理、调度、文件系统、网络、系统调用或用户态服务器。

## 当前状态

已实现：

- RISC-V 64 的 S-mode 内核入口（`kernel/arch/riscv/boot.S`）
- 设置启动栈、清零 BSS
- 链接脚本，把内核放在 `0x80200000`（OpenSBI 之上），并导出内核起止地址
- 轮询式 NS16550A UART 控制台驱动（MMIO `0x10000000`，见 `kernel/arch/riscv/uart.c`）
- 陷阱入口与分发：保存/恢复 32 个整数寄存器，`kernel/interrupt/trap.c` 安装 `stvec`
- 最小异常处理：打印异常名称、`scause`/`sepc`/`stval` 和栈指针后通过 SBI 关机
- 最小时钟中断：经 SBI TIME 扩展每秒重装定时器，前 3 个 tick 打印日志
- 物理内存记录与位图页分配器：从 `0x80000000` 起共 128 MiB，4 KiB 页，内核镜像区间保留
- 启动日志，打印 hello 后进入 `wfi` 等待循环

尚未实现（不要把它们当成已完成）：

- 自研 bootloader：`bootloader/` 目前只有启动方式说明，没有代码；启动完全依赖 QEMU 自带的 OpenSBI 固件（见 `bootloader/README.md`）
- 设备树解析、多 hart、SBI IPI/HSM 等完整平台管理
- 完整的异常/中断管理，以及虚拟内存 / Sv39 页表
- 进程、调度、系统调用、进程间通信
- `userland/` 仍是占位目录；`shared/abi/` 已约定目录用途与编码风格（`shared/README.md`），尚无头文件
- `kernel/capability/cspace.cpp` 仍是 0 字节占位文件，未参与构建；能力与内核对象都还没有实现

## 环境要求与检查

构建使用 **clang** 和 **LLD**，不需要 GCC 交叉工具链。已验证的环境：

| 工具 | 本机情况 |
| --- | --- |
| `qemu-system-riscv64` | 已安装，QEMU 8.2.2（`/usr/bin/qemu-system-riscv64`） |
| OpenSBI 固件 | 已随 QEMU 提供：`/usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin` |
| `clang` | 已安装，18.1.3，可编译 `riscv64-unknown-elf` |
| LLD | 系统 `/usr/bin/ld.lld` 不存在；使用 rustup 工具链自带的 `rust-lld` |
| GCC RISC-V 交叉工具链 | 未安装（本版本不需要） |

检查命令：

```sh
qemu-system-riscv64 --version
clang --version
command -v ld.lld || command -v rust-lld
ls /usr/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin
```

也可以直接运行 `tools/check-env.sh` 完成上述检查；缺少必需依赖时退出码非 0。

如果换一台机器、缺少上述工具，可以参考下面的安装命令（本次没有执行任何安装）：

```sh
# QEMU（Ubuntu/Debian）
sudo apt install qemu-system-misc

# 首选：系统 LLD（提供 /usr/bin/ld.lld）
sudo apt install lld

# 可选：GNU 交叉工具链；当前 Makefile 不直接使用它
sudo apt install gcc-riscv64-unknown-elf binutils-riscv64-unknown-elf
```

Makefile 会按 `ld.lld` → `rust-lld` → `~/.rustup/toolchains/*/lib/rustlib/*/bin/rust-lld`
的顺序自动寻找链接器；找不到时会在构建阶段报错退出。

## 构建

```sh
make
```

产物是 `build/kernel.elf`。也可以运行 `tools/build.sh`（等价于 `make`）。清理：

```sh
make clean
# 或 tools/build.sh clean
```

## 运行

```sh
make run
```

等价于：

```sh
qemu-system-riscv64 -machine virt -m 128M -nographic -bios default -kernel build/kernel.elf
```

在交互终端里按 `Ctrl-A` 然后 `X` 退出 QEMU。脚本中可以这样限时运行：

```sh
timeout 5s qemu-system-riscv64 -machine virt -m 128M -nographic \
    -bios default -kernel build/kernel.elf
```

## 工具脚本（`tools/`）

`tools/` 只放构建、启动和调试脚本，不含内核逻辑。

| 脚本 | 作用 |
| --- | --- |
| `tools/check-env.sh` | 检查 `qemu-system-riscv64`、clang、LLD、clang 的 RISC-V 交叉编译能力和 OpenSBI 固件 |
| `tools/build.sh` | 调用 Makefile 编译 `build/kernel.elf`；`tools/build.sh clean` 清理 |
| `tools/run-qemu.sh` | 编译后用 `-nographic` 启动 QEMU，额外参数透传给 QEMU |
| `tools/debug.sh` | 用 `-s -S` 启动 QEMU 并连接 GDB，串口输出写入 `build/qemu-debug.log` |

`tools/debug.sh` 连接后默认停在复位向量，可先 `break kmain` 再 `continue`；GDB 退出后
脚本会关闭 QEMU。

## 启动路径

1. QEMU `virt` 机器从 `0x80000000` 启动 `-bios default` 指定的 OpenSBI 固件。
2. OpenSBI 初始化完成后，以 **S-mode** 跳转到内核入口 `0x80200000`（`_start`），
   此时 `a1` 指向设备树地址（当前未解析）。
3. `_start`：屏蔽 S-mode 中断，设置 16 KiB 启动栈，清零 `.bss`，调用 `kmain`。
4. `kmain`：安装 `stvec`，初始化物理页分配器，经 SBI 配置首次时钟比较值，然后打开
   `sstatus.SIE`；打印 hello 后进入 `wfi` 循环，之后每秒收到一次 S-mode 时钟中断。

控制台是 QEMU `virt` 上的 NS16550A 兼容 UART，基地址 `0x10000000`；`-nographic`
会把它接到当前终端。更细的地址约定、入口寄存器状态和后续自研 bootloader 的边界，
见 [`bootloader/README.md`](bootloader/README.md)。

## 目录结构

```text
Makefile
kernel/
  boot/boot.c            启动编排：打印日志、初始化、进入 wfi
  console.h              控制台接口
  arch/riscv/
    boot.S               启动入口、设栈、清 BSS
    trap.S               陷阱入口，保存/恢复整数寄存器
    trap.h               陷阱帧与 stvec 接口
    csr.h                最小 CSR 读写辅助
    sbi.c / sbi.h        SBI TIME / SRST 调用
    linker.ld            链接脚本（0x80200000）
    uart.c               NS16550A UART 驱动
  interrupt/trap.c       陷阱分发与中断使能
  exception/exception.c  异常打印与关机
  clock/clock.c          最小时钟中断
  memory/pmm.c           物理内存范围记录与位图页分配
  sched/ syscall/ ipc/   空目录，等用户态和进程接入
  capability/ objects/   能力与内核对象占位，尚未实现
bootloader/              启动方式说明，当前不使用自研 bootloader
shared/                  ABI 约定（见 shared/README.md），abi/ 尚无头文件
tools/                   构建、启动和调试脚本（不含内核逻辑）
  check-env.sh           检查 qemu-system-riscv64 与 RISC-V 工具链
  build.sh               编译内核（封装 make）
  run-qemu.sh            一键编译并用 -nographic 启动 QEMU
  debug.sh               QEMU (-s -S) + GDB 调试
userland/                用户态规划目录，第一版不实现，详见下节
  init/                  1 号用户进程，拉起其他服务
  drivers/               设备驱动
    storage/             存储驱动
  servers/               系统服务
    pm/                  进程管理
    vm/                  虚拟内存
    vfs/                 文件系统
    net/                 网络
    sm/                  服务管理
  libs/                  用户态库和系统调用封装
  apps/                  测试程序和以后的应用
```

## 用户态规划（`userland/`，第一版不实现）

`userland/` 目前只保留目录和职责划分，没有源码、构建规则或可运行内容。
等内核能进入用户态、系统调用可用之后，再先做一个最小 `init` 和 hello 应用。

| 目录 | 规划职责 |
| --- | --- |
| `userland/init/` | 1 号用户进程，负责拉起其他服务 |
| `userland/drivers/` | 设备驱动，例如 `storage/` 下的存储驱动 |
| `userland/servers/` | 系统服务：`pm`（进程管理）、`vm`（虚拟内存）、`vfs`（文件系统）、`net`（网络）、`sm`（服务管理） |
| `userland/libs/` | 用户态库和系统调用封装 |
| `userland/apps/` | 测试程序和以后的应用 |

落地顺序：

1. 内核进入 U-mode，提供最小系统调用（控制台输出、进程退出等）。
2. 实现最小 `init`：启动后打印信息，并拉起 hello 应用。
3. 实现 hello 应用，验证系统调用往返。
4. 之后再逐步补充 `libs/`、`servers/` 和 `drivers/`。

## 实测输出

在 QEMU 8.2.2 + OpenSBI v1.3 下，内核部分输出如下：

```text
[boot] operating-systems-lab: minimal RISC-V 64 kernel
[boot] boot path: QEMU virt -> OpenSBI -> S-mode entry
[boot] stack set up, BSS cleared
[boot] console: NS16550A UART at 0x10000000
[trap] stvec installed at 0x8020089c
[mem] physical memory: 0x80000000 - 0x88000000 (128 MiB)
[mem] kernel ends at 0x80206000, page allocator ready with 32250 free pages
[mem] selftest: allocated 0x80206000 and 0x80207000
[mem] selftest: pages returned, 32250 free pages
[clock] timer armed, interval 1 s (timebase 10 MHz)
[boot] hello from the kernel
[boot] no scheduler, syscalls or virtual memory yet
[boot] entering wfi loop (Ctrl-A X to exit QEMU)
[clock] tick 1
[clock] tick 2
[clock] tick 3
```

页分配器当前按 `-m 128M` 硬编码内存范围；修改 QEMU 内存大小时需要同步
`kernel/memory/memory.h` 中的 `PHYS_MEM_SIZE`，设备树解析会在后续版本替代它。

## 下一阶段建议

1. 解析 OpenSBI 传入的设备树，按实际内存范围替代硬编码的 128 MiB。
2. 启用 Sv39 分页，建立内核直接映射。
3. 完善异常/中断管理，并把时钟中断接入最小调度器。
4. 定义最小系统调用接口，再启动第一个用户态程序（最小 `init` + hello 应用，见“用户态规划”）。
5. 需要独立控制启动流程时，再考虑自研 bootloader 或 M-mode 入口。

`computer-organization-lab` 之类的实验代码不在本仓库范围内，后续如需对接可以在
独立仓库中处理。
