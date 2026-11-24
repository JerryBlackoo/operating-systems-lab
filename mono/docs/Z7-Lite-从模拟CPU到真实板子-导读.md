# Z7-Lite：从模拟 CPU 到真实板子（导读）

> 定位：以 `computer-organization-lab` 里的模拟 CPU、Cache、MMU、总线和设备模型为“知识地图”，沿着项目目录逐个映射到 Z7-Lite（Zynq-7010）的真实硬件。
>
> 本文是阅读路线和章节地图，不是芯片手册的替代品；也不把模拟结果当成实板结论。

## 1. 先划三条事实边界

| 事实类型 | 主要来源 | 可以怎么用 |
|---|---|---|
| 模型行为 | `computer-organization-lab` 的 C++/SystemC 源码与仿真输出 | 解释概念、做对照实验、提出可检验的问题 |
| 芯片规格 | Zynq-7000 TRM、DS187、板级原理图、教材 | 说明真实部件的能力、地址、引脚和限制 |
| 实板结果 | 串口日志、JTAG 实录 | 只有日志支持时才写“已实测” |

三条规则：

1. **仿真通过 ≠ 硬件已验证。** 模拟器里的 Cache 命中、MMU 缺页、定时器中断，只能证明模型的行为。
2. **功能相似 ≠ 实现相同。** 教学 CPU 的 ALU、五级流水线、Sv32 MMU 与 Cortex-A9 的内部实现不是同一张图。
3. **没有公开资料的内部细节，标成“资料未披露/概念模型”。** 不根据模拟器结构臆测 Cortex-A9 的门级或信号级实现。

### 当前实板状态（只按已有记录）

- **SD + U-Boot 路线**：记录为已跑通。内核加载到 `0x00100000`，入口 `0x00100020`；`fatload` 后执行 `dcache flush`、`icache flush`，再 `go`。详见 [Z7-Lite-SD卡经U-Boot启动内核实录.md](Z7-Lite-SD卡经U-Boot启动内核实录.md)。
- **JTAG 路线**：链路、FT232H 和两个 Cortex-A9 目标已确认；通过 JTAG 下载内核、断点调试尚未完成。详见 [Z7-Lite-JTAG连接与调试实录.md](Z7-Lite-JTAG连接与调试实录.md)。
- **`undefined instruction`**：已有“`dcache flush` + `go 0x00100020`”的记录，但 `0x00100000` 与 `0x00100020` 这个变量没有单独隔离，根因未定。本文不把它写成“已解决”。

## 2. 讲解方法：每章七问

每个章节都以真实项目路径开头，例如 `soc/cache/`，然后按同一套问题展开：

1. **你的模型是什么**：文件夹中的部件、关键文件、输入输出、保存的状态。
2. **这个部件为什么存在**：它解决什么问题，与其他部件怎样连接。
3. **Z7-Lite 上的对应物**：位于 PS、PL，还是开发板外部；哪些是共享的，哪些属于每个核心。
4. **模型与真实硬件的区别**：位宽、指令集、组织方式、替换策略、当前模型的边界。
5. **联系你的 OS**：启动、异常、内存管理或驱动为什么会用到它。
6. **怎么验证**：先看模拟器已有例子；实板只在调试接口和运行条件允许时验证，不能从模拟结果推断硬件已实测。
7. **带着一个问题复述**：用一次具体访问、指令或异常把部件串起来。

### 章节模板

```text
### <部件名>

对应路径：<computer-organization-lab 中的真实路径>
Z7-Lite 对应物：<PS / PL / 板载外设；共享还是每核私有>
事实等级：模型行为 / 芯片规格 / 实板日志
验证状态：已仿真 / 已读手册 / 已实板验证 / 未验证
资料依据：<文件与页码，或“资料未披露”>

1. 模型是什么
2. 为什么存在
3. Z7-Lite 上的对应物
4. 模型与硬件的区别
5. 联系 OS
6. 怎么验证
7. 复述问题
```

## 3. 章节地图

### 第一部分：先认识芯片和两个“脑中模型”

| 路径入口 | 从模拟项目出发 | 对应 Z7-Lite 的讲解 |
|---|---|---|
| [soc/](../../computer-organization-lab/soc/README.md) | 模拟系统怎样把 CPU、Cache、控制器组合起来 | 开发板、Zynq 芯片、PS、PL 之间是什么关系 |
| [soc/cpu16/](../../computer-organization-lab/soc/cpu16/README.md) | 16 位单总线、多周期 CPU | 用已有模型理解真实处理器；明确它不是 Cortex-A9 的内部实现图 |
| [soc/rv32/](../../computer-organization-lab/soc/rv32/) | 32 位 RISC-V 多周期与流水线模型 | 与 32 位 ARMv7-A 比较：都执行指令，但指令集、寄存器和特权机制不同 |
| [soc/soc.hpp](../../computer-organization-lab/soc/soc.hpp) | 模拟 SoC 的部件装配 | Zynq 的 PS、片上互连、PL 及板级外设的边界 |

这一部分先回答：**板子、芯片、ARM 核心、FPGA 逻辑分别是什么；哪些部件在芯片内，哪些焊在板子上。**

### 第二部分：CPU 内部——一条指令怎样执行

| 路径入口 | 重点 |
|---|---|
| [soc/cpu16/registers/](../../computer-organization-lab/soc/cpu16/registers/) | GPR、PC、IR、MAR、MDR、FR 等在教学模型中的作用；对照 ARM 可见寄存器、SP、LR、PC 和状态寄存器 |
| [soc/cpu16/alu/](../../computer-organization-lab/soc/cpu16/alu/)、[soc/rv32/datapath/dedicated/](../../computer-organization-lab/soc/rv32/datapath/dedicated/) | ALU、运算输入、结果和标志；对照 Cortex-A9 的整数运算能力，避免把教学数据通路当成芯片内部接线图 |
| [soc/shared/decoder/](../../computer-organization-lab/soc/shared/decoder/) | 机器码怎样被理解；区分项目的 RISC-V/教学编码与 ARM 指令 |
| [soc/shared/control_unit/](../../computer-organization-lab/soc/shared/control_unit/)、[soc/rv32/control_unit/](../../computer-organization-lab/soc/rv32/control_unit/) | 控制信号、硬布线和微程序模型；这些是理解控制的模型，不臆测 Cortex-A9 内部控制电路 |
| [soc/rv32/pipeline/](../../computer-organization-lab/soc/rv32/pipeline/) | 五级流水线、冒险、转发、阻塞；再介绍 Cortex-A9 的乱序、推测、超标量特点与公开资料的边界 |
| [soc/rv32/csr/](../../computer-organization-lab/soc/rv32/csr/)、[soc/rv32/privilege.hpp](../../computer-organization-lab/soc/rv32/privilege.hpp) | RISC-V 特权状态如何控制机器；对照 ARM 模式、CPSR、系统控制寄存器，强调两套架构不能直接套用 |

会选一条简单的算术指令和一条加载指令，沿模型走一遍，再讲 ARM 上同类功能的路径。芯片内部没有公开到门级或信号级的细节，明确标记为**概念模型或资料未披露**。

### 第三部分：地址、Cache 和内存层次

| 路径入口 | 重点 |
|---|---|
| [soc/shared/mmu/](../../computer-organization-lab/soc/shared/mmu/README.md) | 虚拟地址、物理地址、权限、缺页或访问异常；区分教学 MMU、项目 Sv32 与 ARMv7-A 短描述符页表 |
| [soc/shared/mmu/tlb/](../../computer-organization-lab/soc/shared/mmu/tlb/) | TLB 表项、命中与失效；对应 Cortex-A9 的指令/数据 micro-TLB 和主 TLB |
| [soc/shared/mmu/page_table_walker/](../../computer-organization-lab/soc/shared/mmu/page_table_walker/) | 页表遍历与硬件、操作系统各自负责什么 |
| [soc/cache/](../../computer-organization-lab/soc/cache/README.md) | Cache 行、组、Tag、命中/缺失、替换、写策略；对应每核私有 L1 指令/数据 Cache 与共享 L2 |
| [soc/controller/memory/](../../computer-organization-lab/soc/controller/memory/README.md) | CPU 请求如何走向内存控制器；区分模拟器的配置和 Zynq DDR 控制器 |
| [dimm/](../../computer-organization-lab/dimm/) | 芯片、Rank、Bank、行列、读写与刷新；对照板载 DDR3，并说明板上内存颗粒与 PC DIMM 外形不同 |

这一部分用一条贯穿的例子：

```text
ARM 执行一次内存读取
    → 地址转换 / 权限检查
    → Cache 查询
    → 必要时经过互连和 DDR 控制器
    → 板载 DDR3
    → 数据返回处理器
```

同时把 **TLB 缺失、页表权限错误、Cache 缺失、DDR 访问**分开，不把它们混成一种“缺页”。

### 第四部分：总线、控制器和板载外设

| 路径入口 | 重点 |
|---|---|
| [interconnect/](../../computer-organization-lab/interconnect/) | 设备地址窗口、请求与响应、互连的作用；对照 Zynq 的 AXI 和 PS/PL 连接 |
| [soc/controller/interrupt/](../../computer-organization-lab/soc/controller/interrupt/)、[soc/rv32/interrupt_system/](../../computer-organization-lab/soc/rv32/interrupt_system/) | 设备中断如何进入 CPU；对照 ARM 的 GIC 和异常入口 |
| [devices/timer/](../../computer-organization-lab/devices/timer/)、[devices/clint/](../../computer-organization-lab/devices/clint/) | 定时器、中断和时钟节拍；解释 RISC-V CLINT 与 Cortex-A9 私有定时器、全局定时器和 GIC 的区别 |
| [devices/uart/](../../computer-organization-lab/devices/uart/) | UART 的 TX/RX、寄存器、波特率；联系板载 CH340 与 `/dev/ttyUSB0` |
| [devices/common/dma_bus.hpp](../../computer-organization-lab/devices/common/dma_bus.hpp) | DMA 为什么能搬运数据、与 Cache 一致性有什么关系 |
| [dimm/](../../computer-organization-lab/dimm/) 与 SD、QSPI 资料 | 对比 DDR、片上 OCM、SD 卡和 QSPI：谁是运行内存，谁保存启动文件 |
| [devices/nic/](../../computer-organization-lab/devices/nic/) | 网络控制器模型与板载以太网接口的概念对应 |
| [devices/gpu/](../../computer-organization-lab/devices/gpu/)、[devices/nvme/](../../computer-organization-lab/devices/nvme/)、[devices/hdd/](../../computer-organization-lab/devices/hdd/)、[interconnect/pcie/](../../computer-organization-lab/interconnect/pcie/)、[soc/controller/iommu/](../../computer-organization-lab/soc/controller/iommu/) | 作为**概念扩展**介绍；逐项核实是否有板载或芯片级对应，不能因为项目有模型就说 Z7-Lite 板上有这些部件 |

这里特别讲清楚：**UART 是串口通信；JTAG 是调试通道；SD 和 QSPI 是不同的启动存储；PL 不是另一颗通用 CPU。**

### 第五部分：从上电到你的 OS

| 路径入口 | 重点 |
|---|---|
| [simulation/boot/](../../computer-organization-lab/simulation/boot/) | 模拟器怎样加载程序；对照板子的 BootROM、FSBL 和启动镜像 |
| [simulation/simulator.hpp](../../computer-organization-lab/simulation/simulator.hpp) | 模拟器如何推进机器；区别于真实硬件持续运行 |
| [rtl/](../../computer-organization-lab/rtl/) | C++ 功能模拟和可综合 RTL 的区别；介绍 PL 能承载什么 |
| [kernel/arch/arm/](../kernel/arch/arm/) | ARM 入口、异常向量、UART、GIC、定时器与页分配器怎样对应前面介绍的部件 |
| [Z7-Lite-JTAG连接与调试实录.md](Z7-Lite-JTAG连接与调试实录.md) | 用已有实测把 JTAG、两个 ARM 核、寄存器和启动状态串起来 |

两条路线：

```text
教学路线：模拟器加载程序 → CPU 模型执行 → 观察部件状态

实板路线：上电 → BootROM → FSBL → U-Boot 或内核 → UART 输出
```

最后回到 demo，回答四个问题：

1. **内核文件放在哪里？** SD 卡 FAT 分区、QSPI，还是 JTAG 直接下载到 DDR。
2. **由谁加载？** BootROM、FSBL、U-Boot，还是调试器。
3. **从哪里开始执行？** 当前是 `0x00100000` 处的向量表，`_start` 在 `0x00100020`。
4. **错误时谁负责报告？** 串口、JTAG、异常向量，还是完全静默。

## 4. 建议的文档组织

建议在 `computer-organization-lab/docs/` 下建立专题目录，逐章展开：

```text
computer-organization-lab/docs/Z7-Lite-从模拟CPU到真实板子/
├── 00-总览与阅读路线.md
├── 01-soc-从模拟SoC认识Zynq.md
├── 02-soc-cpu16-教学CPU与ARM.md
├── 03-soc-rv32-指令集与执行.md
├── 04-soc-registers-寄存器.md
├── 05-soc-pipeline-流水线.md
├── 06-soc-shared-mmu-MMU与TLB.md
├── 07-soc-cache-缓存层次.md
├── 08-soc-controller-memory与dimm-DDR.md
├── 09-interconnect-互连与总线.md
├── 10-interrupt与timer-中断和定时器.md
├── 11-devices-uart-串口与调试.md
├── 12-devices-板载接口与扩展模型.md
├── 13-simulation-启动与OS.md
└── 14-实验记录与问题索引.md
```

说明：

- 本文是 `operating-systems-lab/docs/` 下的总入口；上面的 14 个章节文件目前是**计划结构**，尚未创建，不先放空文件占位。
- 每篇开头都写**对应的真实项目路径**，正文再按子文件夹展开；文档名不等于项目已有目录。
- 随着某一部分真正开始学习，再创建对应章节，并在本文末尾补链接。

## 5. 推荐阅读顺序与小练习

推荐顺序：

```text
soc/ 总览
  → 寄存器与指令
  → MMU / TLB
  → Cache
  → DDR
  → 中断与 UART
  → 启动 OS
```

每讲完一块，做一个很小的追踪，并要求自己说明“这个结论来自模型、手册还是实板日志”：

1. **一次读命中**：给出虚拟地址，走完 TLB → 页表 → Cache → 返回数据，标出每一步的模型文件和真实硬件对应物。
2. **一次读缺失**：同上，但 L1 缺失、L2 命中；再对比 L2 也缺失、访问 DDR 的路径。
3. **一次定时器中断**：从私有定时器到期，到 GIC、异常向量、处理函数、EOI，逐段对应 OS 代码。
4. **一次启动**：从 BootROM 到 FSBL、U-Boot、内核入口，说明每一段谁配置了 UART、DDR 和页表。

## 6. 与 OS 项目的接口

| OS 文件 | 对应硬件部件 | 关联章节 |
|---|---|---|
| [boot.S](../kernel/arch/arm/boot.S) | 复位入口、栈、VBAR、BSS | 启动、异常 |
| [vectors.S](../kernel/arch/arm/vectors.S) | ARM 异常向量 | GIC、异常 |
| [irq.c](../kernel/arch/arm/irq.c) | GIC-400 分发器与 CPU 接口 | 中断 |
| [clock.c](../kernel/arch/arm/clock.c) | Cortex-A9 私有定时器 | 定时器、调度节拍 |
| [uart.c](../kernel/arch/arm/uart.c) | Cadence UART0 | 串口、调试 |
| [linker.ld](../kernel/arch/arm/linker.ld) | 加载地址 `0x00100000` | 启动、内存布局 |
| [pmm.c](../kernel/memory/pmm.c) | 物理页分配 | MMU、DDR |

当前状态：ARM 构建和 QEMU 运行正常；实板 SD + U-Boot 路线有通过记录；JTAG 调试链路未完成。三者不要混写成“全部已验证”。

## 7. 资料索引

### 教材

| 资料 | 相关章节 | 用途 |
|---|---|---|
| [逻辑篇](file:///media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/Part1_Z7_Lite系列教程之逻辑篇%20V1.1.pdf) | 第 5 章 40–61 页；第 10 章 104–126 页；第 11 章 128–144 页；第 12 章 145–157 页 | Vivado 流程、LED、PLL、RAM、FIFO |
| [SDK 篇](file:///media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/Part2_Z7-Lite系列教程之SDK篇%20V1.1.pdf) | 第 1 章 12–43 页；1.4 节 38–40 页；第 2 章 44–55 页；第 10 章 173–203 页 | Hello World、MIO、启动跳线、FSBL/BOOT.bin |
| [Linux 篇](file:///media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/Part4_Z7-Lite系列教程之Linux系统篇%20V1.1.pdf) | 第 5 章 48–74 页；第 6 章 75–92 页 | SD 启动、BOOT.bin 组成、QSPI 启动 |
| [开箱测试](file:///media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/01_微相Z7-lite%20ZYNQ开发板开箱检测/Z7-Lite_开箱测试_出货不带SD卡%20V1.2.pdf) | SD 卡制作、串口、启动测试 | 板级上手流程 |

### 原理图

`02_Z7-Lite硬件资料/Z7-LITE_Rev1_1.pdf`：

- 第 2–3 页：PS MIO、DDR、QSPI、SD 等连接。
- 第 10 页：UART Type-C、CH340E、SD 卡槽、按键、LED。
- 第 12 页：USB-JTAG（FT232H）。
- 第 14 页：两个 Type-C 口的 5V 汇流与电源。

### 数据手册

`05_Datasheet/`：

- `ug585-Zynq-7000-TRM.pdf`：PS、BootROM、DDR 控制器、UART、GIC、SD/QSPI。
- `ds187-XC7Z010-XC7Z020-Data-Sheet.pdf`：器件规格与引脚。
- `ug821-zynq-7000-swdev.pdf`：BootROM、FSBL、启动镜像。
- `2Gb_DDR3_SDRAM.pdf`：DDR3 颗粒。
- `w25q128fv.pdf`：QSPI Flash。
- `RTL8201F-VB-CG.PDF`：以太网 PHY。
- `C99652_CH340E_2020-08-19.PDF`：USB-UART 桥。

### 仓库与实板记录

- [soc/README.md](../../computer-organization-lab/soc/README.md)：模拟 SoC 的部件导航。
- [README.md](../README.md)：OS 项目的当前状态；其中“ARM 实机待验证”已被 SD 实录更新，阅读时以实录为准。
- [Z7-Lite-SD卡经U-Boot启动内核实录.md](Z7-Lite-SD卡经U-Boot启动内核实录.md)：SD + U-Boot 的完整命令、地址和串口现象。
- [Z7-Lite-JTAG连接与调试实录.md](Z7-Lite-JTAG连接与调试实录.md)：JTAG 链路、OpenOCD、两个 A9 目标。

## 8. 已发现的矛盾与坑

写章节时不要直接复制教材结论，先对照原理图和实板记录：

1. **README 状态过时**：README 写“ARM 实机待验证”，SD 实录已有通过记录。
2. **PLL 章节的 200 MHz 差分时钟**：那是其他板子的配置；Z7-Lite 原理图是 50 MHz 单端时钟到 `N18`。
3. **UART 编号与引脚**：SDK 教材一处写“UART1”，另一处按 MIO14/15 讲 UART0；不要按文字去改引脚复用，以原理图和 FSBL 配置为准。
4. **U-Boot 型号字符串**：打印 `Xilinx Zynq ZC702` 不代表板子型号错，是 U-Boot 配置里的机型字符串。
5. **`go` 与 Cache**：`fatload` 后直接 `go` 可能因 D-cache 未回写而执行到错误内容；实录使用 `dcache flush` + `icache flush`。但 `0x00100000` 与 `0x00100020` 的变量没有单独隔离。
6. **串口复位**：重新打开串口可能触发 DTR/RTS 复位；自动化时必须单进程保持串口打开。
7. **JTAG 初始化脚本**：`ps7_init.tcl` 是 Xilinx xsdb 语法（`mwr`/`mask_write`/`mask_poll`），不能直接在 OpenOCD 里 `source`。
8. **独立 BOOT.BIN**：需要 FSBL + `bootgen`（Xilinx 工具）；没有 Vivado/SDK 时，SD + U-Boot 是当前唯一可行的免 Vivado 路线。
9. **QSPI 烧录**：需要 SDK/Vivado 的 Program Flash，暂时不要动；`saveenv` 也可能写 QSPI。
10. **供电**：板子没有独立电源口，由 UART/JTAG 的 Type-C 供电；两个口经过二极管汇流，单口也能供电，但总负载限制约 500 mA。

## 9. 维护规则

- 每章开头记录：对应路径、Z7-Lite 对应物、事实等级、验证状态、资料页码。
- 没有串口或 JTAG 日志，不写“实板已验证”。
- 模拟器行为、芯片手册规格、实板结果分栏写，不混成一句结论。
- 新增实板日志后，同步更新本文的“当前实板状态”和两份实录。
- 章节文件真正开始写时，再在本文第 4 节补链接，不提前创建空文件。
