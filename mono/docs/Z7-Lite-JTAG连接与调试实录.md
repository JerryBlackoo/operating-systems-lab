# Z7-Lite Z7010：JTAG 连接与调试实录

记录日期：2026-11-19  
目标：验证 Ubuntu 能否通过 JTAG 连接 Zynq-7010 的 ARM 内核，并为后续移植本项目的操作系统做准备。

> 后续更新（2026-11-19）：SD 卡 + U-Boot 启动本项目 ARM 内核已实测通过，见 [Z7-Lite-SD卡经U-Boot启动内核实录.md](Z7-Lite-SD卡经U-Boot启动内核实录.md)。本文的 JTAG 结论仍有效；“尚未从 SD 卡启动”已被后续实验取代。

## 1. 这次的结果

**已实测通过：**

1. 电脑识别板载 FT232H USB-JTAG 接口。
2. OpenOCD 读到了 Zynq 的 JTAG 调试链，包括 Xilinx 器件和 ARM 调试接口。
3. OpenOCD 识别到两个 Cortex-A9 调试目标，初始都显示为 `running`。
4. 暂停处理器后读到了程序计数器、栈指针和处理器状态寄存器。
5. 执行恢复后，两个核心都再次显示为 `running`。

**截至本文 JTAG 实验时尚未完成：**没有通过 JTAG 下载项目内核，没有初始化和测试 DDR，没有验证断点或单步，没有从 SD 卡启动，也没有写入 SD 卡或板载闪存。CPU 显示 `running` 只说明调试器观察到它处于运行状态，不能证明自己的内核或 Linux 已经启动。

> 后续（2026-11-19）：SD 卡 + U-Boot 启动本项目 ARM 内核已实测通过；JTAG 下载仍未完成。详见上方链接。

## 2. 板子和电脑怎样连接

Z7-Lite 的 Zynq-7010 可以先分成两部分理解：

```text
Zynq-7010
├── PS（处理系统）
│   ├── 两个 ARM Cortex-A9 核心
│   ├── DDR、UART、SD 等控制器
│   └── ARM 调试接口
└── PL（可编程逻辑 / FPGA）
```

这次调试的数据路径：

```text
Ubuntu 上的 OpenOCD
        │ USB 数据线
        ▼
开发板上标有 JTAG 的 Type-C 接口
        ▼
板载 FT232H USB-JTAG 接口芯片
        │ TCK / TMS / TDI / TDO
        ▼
Zynq JTAG 调试链 → ARM 调试接口 → Cortex-A9
```

Z7-Lite 上还有标为 **UART** 的 Type-C 接口：它用于程序的串口输入输出；标为 **JTAG** 的接口用于本次调试。只有一根线时，可以按需要换接口；换线期间不要假定还能同时看到串口日志。

这次板子设为 **JTAG 启动模式**。启动模式控制上电时从哪里启动；切换跳线应先断电，再设置并重新上电。JTAG 模式下 UART 没有输出，不足以判断板子故障。

板子的参考原理图：

`/media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/02_Z7-Lite硬件资料/Z7-LITE_Rev1_1.pdf`

其中 USB-JTAG 电路显示 FT232H 的 ADBUS0～3 分别连接 TCK、TDI、TDO、TMS。本次使用的配置只驱动这组 JTAG 信号，不主动操作复位引脚。

## 3. 这次的环境和准备

- 主机：Ubuntu，用户 `jerry`。
- 板子资料目录：`/media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/`。
- 启动方式：JTAG；USB 数据线接板上的 JTAG 接口。
- 调试软件：OpenOCD 0.12.0，Ubuntu 软件包，提取到 `/tmp/z7-os-tools/root`；本次没有将它安装为系统软件。
- 调试配置：OpenOCD 自带的 `target/zynq_7000.cfg`，FT232H 接口引脚按本板原理图设置。
- 权限：当前会话用户不直接具备设备访问权限，本次以 `sudo -n` 启动 OpenOCD。
- JTAG 时钟：1 MHz。

`/tmp` 是临时目录，之后可能被系统清理；不要把它视为项目长期依赖。若工具已清除，可在有网络的 Ubuntu amd64 主机上重新下载、解包到临时目录：

```sh
mkdir -p /tmp/z7-os-tools
cd /tmp/z7-os-tools
apt -o Acquire::ForceIPv4=true download \
  openocd libcapstone4 libgpiod2t64 libhidapi-hidraw0 libjaylink0 libjim0.82t64
for f in ./*.deb; do dpkg-deb -x "$f" ./root; done
```

这些依赖名称适用于本次 Ubuntu 环境，其他发行版、版本或架构可能不同。下载和解包不会自动安装软件包；仍需系统提供 OpenOCD 的其他动态库。也可以自行使用系统正常安装的 OpenOCD，调整下面的路径。

## 4. 复现：先确认电脑发现 USB 接口

```sh
lsusb -d 0403:6014
```

本次输出：

```text
ID 0403:6014 Future Technology Devices International, Ltd FT232H Single HS USB-UART/FIFO IC
```

`lsusb` 只能确认 USB 接口被枚举，**不能单独证明** JTAG 链和 ARM 处理器可用。将线插到 UART 口时，这块板在本次系统上识别为 CH340（`1a86:7523`），而不是这个 FT232H。

## 5. 复现：扫描调试链，不下载程序

本次使用的低层扫描命令：

```sh
sudo -n env LD_LIBRARY_PATH=/tmp/z7-os-tools/root/usr/lib/x86_64-linux-gnu \
  /tmp/z7-os-tools/root/usr/bin/openocd \
  -c 'adapter driver ftdi; ftdi vid_pid 0x0403 0x6014; ftdi layout_init 0x0008 0x000b; transport select jtag; adapter speed 1000; reset_config none; jtag newtap zynq_pl bs -irlen 6 -ircapture 0x01 -irmask 0x03; jtag newtap zynq cpu -irlen 4 -ircapture 0x01 -irmask 0x0f; init; scan_chain; shutdown'
```

含义：

- `adapter driver ftdi`：使用 FTDI 接口驱动。
- `ftdi vid_pid 0x0403 0x6014`：选择本次识别到的设备类型；有多块同类设备时还需进一步指定设备。
- `ftdi layout_init 0x0008 0x000b`：设置 FT232H 低位引脚的初始电平及方向，以本板 JTAG 引脚连接为依据。其他板子不应直接照搬。
- `transport select jtag`、`adapter speed 1000`：选择 JTAG，时钟设置为 1 MHz。
- `reset_config none`：不通过适配器复位引脚控制目标；JTAG 扫描仍会操作调试链状态。
- 两条 `jtag newtap`：描述链上的 Xilinx 和 ARM TAP；`init; scan_chain` 初始化并扫描。

本次关键输出：

```text
Info : JTAG tap: zynq_pl.bs tap/device found: 0x13722093
Info : JTAG tap: zynq.cpu tap/device found: 0x4ba00477
```

第一次只定义了 TAP，没有配置 CPU 目标，OpenOCD 给出 `gdb services need one or more targets defined` 提示；该提示不影响这一步的链扫描结果。

## 6. 复现：识别 ARM 核心

下面加载 OpenOCD 自带的 Zynq-7000 目标配置，查看目标状态：

```sh
sudo -n env LD_LIBRARY_PATH=/tmp/z7-os-tools/root/usr/lib/x86_64-linux-gnu \
  /tmp/z7-os-tools/root/usr/bin/openocd \
  -s /tmp/z7-os-tools/root/usr/share/openocd/scripts \
  -c 'adapter driver ftdi; ftdi vid_pid 0x0403 0x6014; ftdi layout_init 0x0008 0x000b; transport select jtag; reset_config none' \
  -f target/zynq_7000.cfg \
  -c init -c targets -c 'zynq.dap info 0' -c shutdown
```

本次关键输出：

```text
Info : zynq.cpu0: hardware has 6 breakpoints, 4 watchpoints
Info : zynq.cpu1: hardware has 6 breakpoints, 4 watchpoints

TargetName   Type       State
zynq.cpu0    cortex_a   running
zynq.cpu1    cortex_a   running
```

这里的断点和观察点数量是调试器报告的硬件能力；**本次没有实际设置断点或观察点**。DAP 信息命令报告 AP 0 为 AHB3 MEM-AP，但仅凭此不能认定 DDR 已初始化或所有外设都可正常访问。

## 7. 复现：暂停、读取寄存器、恢复

本次先确认两个核心均为运行状态，再进行了下面的操作：

```sh
sudo -n env LD_LIBRARY_PATH=/tmp/z7-os-tools/root/usr/lib/x86_64-linux-gnu \
  /tmp/z7-os-tools/root/usr/bin/openocd \
  -s /tmp/z7-os-tools/root/usr/share/openocd/scripts \
  -c 'adapter driver ftdi; ftdi vid_pid 0x0403 0x6014; ftdi layout_init 0x0008 0x000b; transport select jtag; reset_config none' \
  -f target/zynq_7000.cfg \
  -c init -c 'targets zynq.cpu0' -c 'halt 3000' \
  -c 'reg pc' -c 'reg sp' -c 'reg cpsr' \
  -c targets -c resume -c targets -c shutdown
```

由于配置启用了多核调试，`halt` 后这次两个核心都显示为 `halted`；`resume` 后两者都显示为 `running`。调试日志中出现的示例值包括：

```text
target halted in ARM state due to debug-request, current mode: Supervisor
cpsr: 0x000001d3 pc: 0xffffff34
target halted in ARM state due to debug-request, current mode: System
cpsr: 0x200001df pc: 0xffffff28
MMU: disabled, D-Cache: disabled, I-Cache: disabled
```

这些是当时的瞬时状态，不是内核运行成功的证据，也不应作为固定值。调试器还输出了 `reg sp` 的值；多核暂停可能切换调试上下文，分析某个寄存器属于哪个核心时，应显式选择核心并重新读取，不要仅凭混合日志推断。

**注意：暂停会暂时停止目标程序。** 如果板上正在运行重要任务，应先判断影响；若调试器在 `halt` 和 `resume` 之间报错或断线，目标可能仍处于暂停状态，需重新连接、检查状态后处理。不要不加判断地让原本已经暂停的目标运行。

## 8. 与 UART 的关系

UART 是板上程序的文字输入输出渠道；JTAG 是调试和配置通道。

此前使用 UART 接口时，Linux 识别到 `1a86:7523` CH340 及 `/dev/ttyUSB0`。以 115200、8N1 监听、发送回车，并在按复位键期间监听，均没有收到数据。当时 SD 卡仍在电脑读卡器中，启动模式与可执行程序也未完整验证。因此不能据此判断 UART 电路损坏，或推断板上已经运行了特定系统。

> 后续（2026-11-19）：同一 UART（CH340、115200 8N1）已实测可用；当时无输出是 SD 卡/启动状态问题，不是串口电路故障。

要调试自己内核，理想情况下 JTAG 用于检查 CPU 和内存，UART 用于查看日志。只有一根线时需要分阶段切换，不能同时依赖两个接口。

## 9. 与本项目移植的关系及后续工作

本项目原有内核面向 RISC-V 64/QEMU；Zynq-7010 使用 32 位 ARM Cortex-A9，不能直接执行 RISC-V 产物。JTAG 链验证成功只打通了调试通道，后续仍需：

1. 确定 ARM 内核入口、链接地址与加载方式，建立可单独构建的目标。
2. 明确使用片上内存还是已初始化的 DDR；使用 DDR 前先有适配本板的初始化流程。
3. 实现 ARM 启动代码和 Zynq UART，验证最小内核启动及日志。
4. 移植可复用的内存管理逻辑；分别实现 ARM 异常入口、中断控制器与定时器。
5. 根据验证结果选择 JTAG 下载调试，或由 SD 卡上的 FSBL/U-Boot 加载。SD/U-Boot 路线已在 2026-11-19 实测通过；JTAG 下载仍需实际测试，不能把通道识别成功当作内核运行成功。

教材中的 `7010开箱测试images/z7_lite_10.img` 是 7010 启动镜像，包含 FAT32 与 Linux 分区；恢复该镜像会覆盖目标 SD 卡的分区和数据。**本次没有写卡**，使用前应明确核对目标与数据。

## 10. 手动操作：用两个终端交互式调试

前面的第 7 节是一条命令自动完成暂停、读寄存器和恢复。本节把相同操作拆开，方便自己观察每一步。调试服务只绑定本机 `127.0.0.1`。

准备：开发板已上电，USB 数据线连接板上的 **JTAG** 接口，电脑识别到 FT232H；这次实验的启动模式为 JTAG。第 3 节的 OpenOCD 和临时依赖必须仍然存在。`/tmp` 可能在重启或清理后消失，缺失时按第 3 节重新准备。

下面区分两个终端：**终端一运行 OpenOCD，终端二连接到它并输入调试命令**。终端一运行期间保持打开。

### 10.1 终端一：启动 OpenOCD

在 Ubuntu 终端中整段粘贴并回车：

```sh
sudo env LD_LIBRARY_PATH=/tmp/z7-os-tools/root/usr/lib/x86_64-linux-gnu \
  /tmp/z7-os-tools/root/usr/bin/openocd \
  -s /tmp/z7-os-tools/root/usr/share/openocd/scripts \
  -c 'bindto 127.0.0.1' \
  -c 'adapter driver ftdi; ftdi vid_pid 0x0403 0x6014; ftdi layout_init 0x0008 0x000b; transport select jtag; reset_config none' \
  -f target/zynq_7000.cfg \
  -c init
```

如有提示，输入当前 Ubuntu 账户的管理员密码。连接成功时，输出中应能看到类似：

```text
JTAG tap: zynq_pl.bs tap/device found: 0x13722093
JTAG tap: zynq.cpu tap/device found: 0x4ba00477
zynq.cpu0: hardware has 6 breakpoints, 4 watchpoints
zynq.cpu1: hardware has 6 breakpoints, 4 watchpoints
```

OpenOCD 会继续运行并输出日志；这是正常的。不要在这个终端输入下面的 `targets`、`halt` 等命令，也不要同时启动第二个 OpenOCD 占用同一个 USB 接口。

### 10.2 终端二：连接调试命令行

另开一个 Ubuntu 终端，输入：

```sh
telnet 127.0.0.1 4444
```

连接成功后进入 OpenOCD 命令行，提示符通常是 `>`。**下面的命令都在这个提示符下输入，不是输入到普通 shell 中。** Telnet 只连到本机服务；若连接失败，先检查终端一是否仍在运行、是否有错误。

### 10.3 查看状态，选择核心并暂停

在 OpenOCD 的 `>` 提示符下输入：

```text
targets
```

输出示例：

```text
TargetName   Type       State
zynq.cpu0    cortex_a   running
zynq.cpu1    cortex_a   running
```

`running` 表示调试器观察到处理器在运行，不能据此认为自己的内核已启动。先记下两个核心的原始状态；暂停会影响板上正在执行的程序。

选择核心 0 并暂停：

```text
targets zynq.cpu0
halt
targets
```

在本次使用的多核调试配置下，之前实验中 `halt` 使两个核心都变为 `halted`。以实际 `targets` 输出为准。

### 10.4 查看寄存器

处理器暂停后，在同一 OpenOCD 提示符中依次输入：

```text
reg pc
reg sp
reg cpsr
```

也可以用 `reg` 查看当前目标的寄存器列表。`PC` 与执行位置有关，`SP` 是栈指针，`CPSR` 包含 ARM 的状态和模式。多核调试时上下文可能切换；要分析某一个核心，应再次用 `targets zynq.cpu0` 或 `targets zynq.cpu1` 选择目标，并核对输出。

### 10.5 恢复并退出

如果暂停前两个核心都在运行，查看完成后输入：

```text
resume
targets
```

确认核心恢复为 `running`。如果暂停之前有核心已经处于 `halted`，先弄清原来的调试状态，再决定如何处理，不要无条件恢复。

确认状态后，在 OpenOCD 提示符输入：

```text
shutdown
```

这会关闭 OpenOCD 服务，终端二的连接也会结束。若在暂停后发生断线或工具报错，目标可能仍停着；重新连接后先用 `targets` 检查，再根据操作前的状态处理。

### 10.6 常见问题

- **`lsusb -d 0403:6014` 没有输出**：检查是否插在 JTAG 口、板子是否上电、USB 线是否支持数据，以及是否连接到正确电脑。
- **终端一提示找不到程序或共享库**：检查 `/tmp/z7-os-tools/root` 是否还在，并确认命令包含 `LD_LIBRARY_PATH`；依赖可能已被系统清理，见第 3 节。
- **终端一提示 USB 设备不可用或被占用**：检查权限及是否已有 OpenOCD 等程序占用接口；不要同时开多个调试服务。
- **终端二连接被拒绝**：检查终端一是否已成功运行，以及本机 4444 端口是否被占用。
- **没有 UART 输出**：JTAG 通了不代表 UART 已连接或有程序发送日志；串口要连接另一接口，且需要目标程序主动输出。

上述交互步骤整理自本次实测的 OpenOCD 命令；**本次验证采用的是第 7 节批处理，尚未额外执行本节 Telnet 交互会话**。示例输出和寄存器值会随板子状态变化。
