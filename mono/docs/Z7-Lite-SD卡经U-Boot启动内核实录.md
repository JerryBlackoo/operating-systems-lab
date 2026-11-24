# Z7-Lite Z7010：OS Demo 内核部署与复现实录

记录日期：2026-11-19  
状态：demo 内核已通过 SD 卡 + U-Boot 在 Z7-Lite 实机启动；自动启动、独立 BOOT.BIN、JTAG 下载尚未验证。  
关联文档：[Z7-Lite-JTAG连接与调试实录.md](Z7-Lite-JTAG连接与调试实录.md)

> 本文以本次实际串口日志、U-Boot 输出、主机文件哈希和项目当前构建产物为依据。  
> 文中明确区分 **已实测**、**高置信推断** 和 **未验证**；未验证的内容不能当作结论。

## 0. 证据等级

- **已实测**：有命令输出、串口日志、文件哈希或可重复操作支撑。
- **高置信推断**：现象支持，但没有隔离变量，或没有查看 U-Boot 源码/原理图逐脚确认。
- **未验证**：尚未测试的假设或后续方案。

## 1. 开发板结构

### 1.1 Zynq-7010 的整体划分

Z7-Lite 上的 Zynq-7010（XC7Z010）分为 PS（处理系统）和 PL（可编程逻辑）两大部分：

```text
Zynq-7010 (XC7Z010)
├─ PS（Processing System，处理系统）
│  ├─ 双核 ARM Cortex-A9（ARMv7-A，含 NEON/FPU）
│  ├─ DDR 控制器 → 板载 DDR3（U-Boot 报告 512 MiB）
│  ├─ UART0（MIO14/MIO15）→ CH340E → USB Type-C UART
│  ├─ SD0（MIO40-MIO45）→ MicroSD 卡槽
│  ├─ GIC（中断控制器）、私有定时器、全局定时器
│  └─ JTAG 调试接口 → FT232H → USB Type-C JTAG
└─ PL（Programmable Logic，可编程逻辑 / FPGA fabric）
   └─ bitstream 通常由 FSBL 在启动时加载
```

### 1.2 各模块的作用

| 模块 | 作用 | 本次使用情况 |
| --- | --- | --- |
| ARM Cortex-A9 | 运行 FSBL、U-Boot、Linux 或裸机内核 | demo 内核运行在 ARM 核上 |
| DDR | 存放代码、数据、栈、页分配器管理的物理页 | demo 内核链接到 `0x00100000`，PMM 使用到 `0x20000000` |
| UART0 | 串口控制台，115200 8N1 | 通过 CH340 连接主机，查看 U-Boot 和内核日志 |
| JTAG | 处理器调试、暂停/单步/读写寄存器 | 本次部署未使用；见 JTAG 实录 |
| SD 卡 | 启动介质和文件存储 | 从 SD 启动，U-Boot 从 FAT 分区读取内核 |
| PL / FPGA | 可编程逻辑，可做外设、加速器、LED 等 | demo 内核未使用 PL；bitstream 是否加载未直接观察 |
| BootROM | 上电后读取启动模式，加载 FSBL | 已实测能从 SD 启动 |
| FSBL | 初始化 PS（DDR/时钟/MIO），加载下一阶段 | 由 BOOT.BIN 提供；版本未显示 |
| U-Boot | 启动加载器、命令行、加载 Linux 或裸机内核 | 2018.01，提示符 `Zynq>` |
| Linux | 原厂 Debian 系统，用于对照和回退 | 可从 `image.ub` + rootfs 启动 |

### 1.3 本次实际使用的路径

```text
SD 卡（BOOT 分区）
  → BootROM
  → FSBL
  → U-Boot
  → fatload kernel.bin 到 DDR 0x00100000
  → go 0x00100020
  → demo 内核
      ├─ UART0 输出日志
      ├─ GIC 初始化
      ├─ Cortex-A9 私有定时器
      └─ 物理页分配器
```

JTAG 和 PL 不在本次 demo 启动的关键路径上。PL 的 bitstream 在 `BOOT.BIN` 内，但本次没有验证它是否被加载、加载后是否影响 demo 内核。

## 2. SD 卡与启动流程

### 2.1 已确认的卡布局

本次使用的是一张 **58.2 GB** 的 SD 卡，先写入教材的 7010 启动镜像 `z7_lite_10.img`，再把 demo 内核复制到 FAT 分区。

写入后 `lsblk` / `fdisk` 确认：

| 分区 | 大小 | 文件系统 | 标签 | 作用 |
| --- | --- | --- | --- | --- |
| `/dev/sda1` | 96 MiB | FAT32 | `BOOT` | 启动分区，放 `BOOT.BIN`、`image.ub`、`kernel.bin` |
| `/dev/sda2` | 1.4 GiB | ext4 | `rootfs` | 原厂 Debian 8 根文件系统 |
| 剩余 | 约 56.8 GiB | 未分配 | — | 暂未使用 |

FAT 分区实际文件：

```text
BOOT.BIN     2702312 字节
image.ub     4009004 字节
kernel.bin      4528 字节
```

### 2.2 各文件的作用

| 文件 | 作用 | 本次是否使用 |
| --- | --- | --- |
| `BOOT.BIN` | FSBL + bitstream + U-Boot，BootROM/FSBL 启动链的入口 | 使用，保持原样 |
| `image.ub` | FIT 镜像，包含 Linux 4.14.0-xilinx 内核和设备树 | 用于原厂 Debian 启动/回退 |
| `kernel.bin` | 本项目 ARM demo 内核的裸二进制 | 使用，U-Boot 手动加载 |
| rootfs 分区 | Debian 8 根文件系统 | demo 内核不使用；原厂 Linux 使用 |

原厂镜像 `z7_lite_10.img` 的信息：

```text
路径：
/media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/
  01_微相Z7-lite ZYNQ开发板开箱检测/7010开箱测试images/z7_lite_10.img

大小：2147483648 字节（2 GiB）
分区表：DOS/MBR
  partition 1: ID=0xc（FAT32），96 MiB
  partition 2: ID=0x83（Linux），约 1.4 GiB
```

`z7_lite_20.img` 对应 7020，本次未使用。镜像目录里还有独立的 `BOOT.bin` 和 `image.ub`，它们与卡上文件不是同一份，本次没有使用，不能混用。

### 2.3 上电到 U-Boot（已实测）

已实测的 U-Boot 启动日志：

```text
U-Boot 2018.01 (Jul 10 2020 - 07:33:19 +0000) Xilinx Zynq ZC702

Model: Navigator Development Board
Board: Xilinx Zynq
Silicon: v3.1
DRAM:  ECC disabled 512 MiB
MMC:   mmc@e0100000: 0 (SD)
...
In:    serial@e0000000
Out:   serial@e0000000
Err:   serial@e0000000
...
Hit any key to stop autoboot:  4
Zynq>
```

对应过程：

1. 板子上电，BootROM 根据启动模式跳线选择 SD 卡。
2. BootROM 从 FAT 分区读取 `BOOT.BIN`，加载 FSBL。
3. FSBL 初始化 PS（DDR、时钟、MIO 等），加载 U-Boot。
4. U-Boot 初始化串口、SD 控制器等，打印版本和 4 秒倒计时。
5. 倒计时内按键，进入 `Zynq>` 命令行；不按键则继续自动启动。

> 注意：U-Boot 的型号字符串显示 `Xilinx Zynq ZC702`，但实际板子是 Z7-Lite Z7010。这是厂商 U-Boot 使用了 ZC702 配置，不代表板子型号是 ZC702。

### 2.4 U-Boot 到 Debian（已实测）

不按键时，U-Boot 默认加载 `image.ub`：

```text
reading image.ub
4009004 bytes read in 234 ms (16.3 MiB/s)
## Loading kernel from FIT Image at 10000000 ...
   Using 'conf@system-top.dtb' configuration
   Trying 'kernel@1' kernel subimage
     Description:  Linux kernel
     Type:         Kernel Image
     Compression:  gzip compressed
     Data Start:   0x100000f4
     Data Size:    3989234 Bytes = 3.8 MiB
     Architecture: ARM
     OS:           Linux
     Load Address: 0x00008000
     Entry Point:  0x00008000
...
   Booting using the fdt blob at 0x103ce0e8
   Uncompressing Kernel Image ...
```

随后 Linux 启动，串口出现：

```text
Debian GNU/Linux 8 zynq ttyPS0

zynq login:
```

登录后确认：

```text
Linux zynq 4.14.0-xilinx #13 SMP PREEMPT Fri Jul 10 07:32:55 UTC 2020 armv7l
Debian GNU/Linux 8 (jessie)
```

rootfs 挂载为 `/dev/mmcblk0p2`（ext4），FAT 分区为 `/dev/mmcblk0p1`。

### 2.5 U-Boot 到 demo 内核（已实测）

在 `Zynq>` 下手动执行：

```text
fatload mmc 0:1 0x00100000 kernel.bin
md.l 0x00100000 4
crc32 0x00100000 0x11b0
dcache flush
icache flush
go 0x00100020
```

内核成功启动并打印日志（见第 3.6 节）。

### 2.6 尚未核实的启动细节

以下内容本次没有逐项验证，不能当成已确认事实：

- 启动模式跳线的具体引脚位置和电平组合；只按教材说明设为 SD 启动。
- FSBL 的版本、配置和它加载 U-Boot 的具体分区/地址。
- PL bitstream 是否在本次启动中被加载；没有观察 FPGA 状态或 PL LED。
- U-Boot 环境变量保存在哪里（SD、QSPI 还是默认环境）；没有执行 `saveenv`。
- 看门狗（SWDT）在上电后的状态；demo 内核没有喂狗，但本次运行期间没有观察到复位。
- CPU1 的状态；demo 内核没有 SMP 启动代码，未检查 CPU1 是否被停住。
- DDR 的具体型号和时序；教材配置为 MT41J256M16 RE-125、16-bit，但本次只通过 U-Boot 的 512 MiB 报告间接确认。
- 卡上 `BOOT.BIN` 和 `image.ub` 的精确构建版本；只看到文件大小和 U-Boot 版本字符串。

## 3. 实际部署步骤（按时间顺序）

### 3.1 确认 SD 卡并写入原厂镜像

主机上确认设备：

```sh
lsblk -o NAME,SIZE,TYPE,FSTYPE,LABEL,MOUNTPOINT,TRAN,RM
```

本次卡是 `/dev/sda`，58.2 GB，USB，可移动；系统盘是 `nvme0n1`，两者不同。

写入镜像（**会重建整张卡的分区并清除数据**）：

```sh
sudo dd if='/media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/01_微相Z7-lite ZYNQ开发板开箱检测/7010开箱测试images/z7_lite_10.img' \
        of=/dev/sda bs=4M conv=fsync status=progress
sync
```

本次写入约 2 GiB，耗时约 189 秒，最后报告 `2147483648 字节已复制`。

写入后：

```text
sda      58.2G disk
├─sda1     96M part vfat  BOOT
└─sda2    1.4G part ext4  rootfs
```

### 3.2 构建 ARM demo 内核

```sh
cd /home/jerry/Projects/from-scratch/operating-systems-lab/mono
make ARCH=arm bin
```

本次产物：

| 文件 | 大小 | SHA-256 | 说明 |
| --- | --- | --- | --- |
| `build/arm/kernel.bin` | 4528 字节（0x11b0） | `66f48bd8cd829889471fe3ed860dcad4c39dac3ef60c502be0347bf7b32dd0e1` | 裸二进制，加载到 `0x00100000` |
| `build/arm/kernel.elf` | 88888 字节 | `2c53dc757f0884b6c61282baa85449930002ebffde3c92446a34478c0b9a687d` | ELF，entry `0x100020` |

主机侧校验：

```python
import zlib, struct
data = open('build/arm/kernel.bin', 'rb').read()
print('size', len(data), hex(len(data)))
print('crc32', format(zlib.crc32(data) & 0xffffffff, '08x'))
print('first4', ' '.join(format(w, '08x') for w in struct.unpack('<4I', data[:16])))
```

本次输出：

```text
size 4528 0x11b0
crc32 c1a8b40e
first4 ea000006 ea00033c ea00034d ea000340
```

**这些值只对本次构建有效。** 重新编译后必须重新计算，不能直接套用。

### 3.3 把 kernel.bin 复制到 SD 卡

```sh
udisksctl mount -b /dev/sda1
# 如果挂载点不是 /media/jerry/BOOT，用下面命令确认
findmnt -no TARGET /dev/sda1

cp build/arm/kernel.bin /media/jerry/BOOT/
sync
sha256sum build/arm/kernel.bin /media/jerry/BOOT/kernel.bin
udisksctl unmount -b /dev/sda1
udisksctl power-off -b /dev/sda
```

本次 `/media/jerry/BOOT` 内容：

```text
BOOT.BIN     2702312
image.ub     4009004
kernel.bin      4528
```

`BOOT.BIN` 和 `image.ub` 保持原样，用于回退原厂 Linux。

### 3.4 串口连接

本次使用板上的 **UART** Type-C 接口，主机识别为 CH340：

```text
1a86:7523 QinHeng Electronics CH340 serial converter
/dev/ttyUSB0
```

串口参数：

```text
115200 8N1，无流控
```

主机侧使用 pyserial 3.5，以 `sudo python3` 运行（当前用户不在 `dialout` 组）。串口设备稳定路径：

```text
/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0
```

不要把它和 JTAG 的 FT232H（`0403:6014`）混用；两者是不同接口。

### 3.5 进入 U-Boot

如果板子已经启动到 Debian，可以使用教材默认的 root 账号登录（密码见 Linux 系统篇，本次使用出厂默认值），然后热重启：

```text
zynq login: root
Password: <教材默认密码>
root@zynq:~# reboot
```

`reboot` 后立刻在串口连续发送空格/按键，打断 U-Boot 的 4 秒倒计时：

```text
Hit any key to stop autoboot:  4
Zynq>
```

本次还观察到：如果在 U-Boot 已经停在 `Zynq>` 后关闭串口、再用新进程重新打开，板子可能复位并重新启动。见第 4.2 节。

### 3.6 加载并执行内核

在 `Zynq>` 下执行：

```text
fatload mmc 0:1 0x00100000 kernel.bin
md.l 0x00100000 4
crc32 0x00100000 0x11b0
dcache flush
icache flush
go 0x00100020
```

本次关键输出：

```text
reading kernel.bin
4528 bytes read in 13 ms (339.8 KiB/s)

00100000: ea000006 ea00033c ea00034d ea000340

crc32 for 00100000 ... 001011af ==> c1a8b40e

## Starting application at 0x00100020 ...
[boot] operating-systems-lab: minimal kernel
[boot] arch: armv7-a (Zynq-7010, Cortex-A9)
[boot] console: Cadence UART0 at 0xE0000000
[boot] stack set up, BSS cleared
[irq] GIC distributor 0xf8f01000, CPU interface 0xf8f00100
[irq] private timer interrupt 29 enabled
[mem] physical memory: 0x100000 - 0x20000000 (511 MiB)
[mem] kernel ends at 0x10d000, page allocator ready with 130803 free pages
[mem] selftest: allocated 0x10d000 and 0x10e000
[mem] selftest: pages returned, 130803 free pages
[clock] private timer armed: 333333333 ticks (1 s at 333 MHz on Zynq-7010)
[boot] hello from the kernel
[boot] no scheduler, syscalls or virtual memory yet
[boot] entering idle loop
[clock] tick 1
[clock] tick 2
[clock] tick 3
```

最开始可能出现几个乱码字节（U-Boot 到内核 UART 初始化的切换），随后输出稳定；这不是内核崩溃。

### 3.7 退出

- 要退出 demo 内核：按板子复位键或断电重上电。
- 要回到原厂 Debian：重启后不要在 U-Boot 倒计时按键，让它加载 `image.ub`。
- 不要通过串口发送命令来“退出”内核；内核没有实现 shell 或退出机制。

## 4. 问题与排查

### 4.1 `undefined instruction at 0x00100000`

#### 现象

第一次尝试时，U-Boot 已经成功 `fatload`，但直接跳 `0x00100000`：

```text
Zynq> fatload mmc 0:1 0x00100000 kernel.bin
reading kernel.bin
4528 bytes read in 13 ms (339.8 KiB/s)

Zynq> go 0x00100000
## Starting application at 0x00100000 ...
undefined instruction
pc : [<00100000>]  lr : [<1ff3dafc>]
reloc pc : [<e05c4000>]  lr : [<00401afc>]
sp : 1f2bbd88  ip : 00000030  fp : 1ff3dab4
...
Flags: nZCv  IRQs off  FIQs off  Mode SVC_32
Resetting CPU ...

resetting ...
```

随后 U-Boot 复位，再次启动，最后进入 Debian。

#### 已实测的事实

1. `fatload` 确实执行成功，报告 `4528 bytes read`。
2. 另一轮实验里，**没有执行 `fatload`**，直接 `md.l 0x00100000 4`，结果是：

   ```text
   00100000: ffffffff ffffffff ffffffff ffffffff
   ```

3. 正确执行 `fatload` 后，`md.l` 和 `crc32` 与主机文件完全一致：

   ```text
   00100000: ea000006 ea00033c ea00034d ea000340
   crc32 for 00100000 ... 001011af ==> c1a8b40e
   ```

4. 在 `fatload`、`md.l`、`crc32` 之后执行 `dcache flush`、`icache flush`，再从 `0x00100020` 进入，内核成功运行。

#### 高置信推断的原因

- `fatload` 把数据写入 DDR 时经过 D-cache，数据可能仍是 cache 中的脏行，尚未回写到 DDR。
- 这版 U-Boot 的 `go` 在本次没有表现出“自动替应用刷 D-cache”的效果；至少不能依赖它。
- 直接跳转后，指令取指看到的是 DDR 中的旧内容。`ffffffff` 不是合法 ARM 指令，因此报 `undefined instruction`。
- 刷 `dcache` / `icache` 后，指令取指能看到正确的 `ea000006`，于是从 `0x00100020` 进入成功。

#### 尚未验证的假设

- `go 0x00100000` 在刷完缓存后是否也能成功；本次没有在刷缓存后重试这个地址。
- 只刷 `dcache` 或只刷 `icache` 是否足够；本次两个都刷了，没有隔离。
- `md` / `crc32` 是否会影响 cache 状态；它们只是读内存，理论上不应改变脏行状态，但没有做严格隔离实验。
- CPU 是否处于 Thumb 状态；异常帧显示 `Mode SVC_32`，但没有解析 CPSR 的 T 位，无法据此下结论。
- U-Boot 跳转前是否 MMU 已关闭；没有检查 CP15 或 U-Boot 源码。
- `go` 的实现是否在某些配置下会刷缓存；没有查看这版 U-Boot 的源码。

#### 已尝试的排查

1. `md.l 0x00100000 4`：确认内存内容。
2. `crc32 0x00100000 0x11b0`：确认 4528 字节整体一致。
3. `dcache flush` / `icache flush`：刷新缓存。
4. `go 0x00100020`：绕过向量表，直接进入 ELF entry。
5. 保持串口进程不关闭，避免 DTR/RTS 复位干扰。

### 4.2 串口重新打开导致板子复位

#### 现象

- 一个脚本已经抓到 `Zynq>`，脚本退出并关闭串口。
- 下一个脚本重新打开 `/dev/ttyUSB0` 时，板子重新启动，`Zynq>` 消失，最后进入 Debian。
- 串口偶尔报：

  ```text
  device reports readiness to read but returned no data
  (device disconnected or multiple access on port?)
  ```

#### 已实测的事实

- 只要从登录、热重启、抢 U-Boot、`fatload` 到 `go` 全程使用同一个进程、串口不关闭，就能稳定抓到 U-Boot 并启动内核。
- 在抓到 `Zynq>` 后关闭串口、另起进程重新打开，板子会重新启动。

#### 高置信推断的原因

- USB-TTL 的 DTR/RTS 在打开串口时会被拉高或翻转。
- 板上 CH340E 的 `RTS#` 等控制线可能参与复位或启动模式电路；原理图里能看到 CH340E 的 `RTS#`，但没有逐脚追完它到复位电路的连接。
- 因此每次 `open()` 可能相当于给板子一个复位脉冲。

#### 尚未验证的假设

- 具体是 DTR 还是 RTS 触发复位。
- 触发的是复位引脚还是启动模式引脚。
- 是否还叠加了 USB 重新枚举、供电波动等因素。

#### 解决办法

- **全程一个进程持有串口，不要在关键步骤之间关闭/重新打开。**
- 打开后立即释放控制线：

  ```python
  import serial
  p = serial.Serial('/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0',
                    115200, timeout=0.1)
  p.dtr = False
  p.rts = False
  ```

- 如果必须分两个进程，先确认复位是否发生；不要假设 U-Boot 提示符还在。

### 4.3 看不到 U-Boot，直接进入 Debian

#### 已实测的事实

- U-Boot 倒计时只有约 4 秒。
- 不在倒计时内按键，U-Boot 会加载 `image.ub`，最后进入 Debian。
- 登录 Debian 后执行 `reboot`，并在重启期间连续发送空格，可以重新抓到 `Zynq>`。

#### 解决办法

1. 串口终端先连好，115200 8N1。
2. 登录教材默认 root 账号。
3. 执行 `reboot`。
4. 从重启开始连续按空格/任意键，直到出现 `Zynq>`。

#### 未验证

- 按住一个键不放和连续敲击是否有区别。
- 是否可以通过 `bootdelay` 环境变量延长倒计时；没有尝试 `saveenv`。
- `saveenv` 会写到哪里、是否会影响原厂启动；没有验证。

### 4.4 串口完全没数据

#### 现象

115200、57600、9600 都读到 0 字节；`md` 无法执行，因为没有进入 U-Boot。

#### 已实测/已确认

- CH340 在主机上正常枚举，USB 供电 `98mA`，状态 `active`。
- 同一 UART 后来在 115200 下能正常看到 Debian 登录和内核日志。
- 因此当时的“无数据”不是串口电路损坏。

#### 可能原因

- 板子没有上电；CH340 由 USB 供电，板子断电时它仍可能被主机识别。
- 线插在 JTAG 口，不是 UART 口。
- 外接 USB-TTL 的 TX/RX 没有交叉，或没有共地。
- 波特率不对。

#### 排查

```sh
lsusb | grep -E '1a86:7523|0403:6014'
ls -l /dev/ttyUSB0
```

- `1a86:7523`：CH340，串口。
- `0403:6014`：FT232H，JTAG。
- 确认板子电源灯亮，再确认 UART 口。

### 4.5 `md` 显示全是 `ffffffff`

#### 已实测的事实

- 那一轮脚本漏掉了 `fatload`，直接执行了 `md`。
- 补上 `fatload` 后，`md.l` 和 `crc32` 都正确。

#### 结论

- 不是内存坏了，也不是 `kernel.bin` 坏了，是根本没有加载。
- 每次跳转前固定做 `fatload` → `md.l` → `crc32` 三项检查。

### 4.6 `mmc 0:1` 编号不确定

```text
mmc list
```

本次输出：

```text
mmc@e0100000: 0 (SD)
```

所以 SD 是设备 0，FAT 分区是分区 1，命令用 `mmc 0:1`。如果换板或换 U-Boot，先重新执行 `mmc list`。

### 4.7 `dcache` / `icache` 命令是否存在

这版 U-Boot 的字符串里包含：

```text
dcache
icache
bootelf
```

在 `Zynq>` 下执行 `dcache flush`、`icache flush` 均被接受。`bootelf` 也存在，但本次没有使用。

### 4.8 其他尚未验证的假设

- **看门狗复位**：demo 内核没有喂狗，但本次运行期间没有观察到复位；SWDT 状态未检查。
- **Thumb 状态**：异常帧没有解析 CPSR T 位；不能排除第一次跳转时 CPU 状态不对。
- **MMU 状态**：没有确认 U-Boot 跳转前是否关闭 MMU。
- **U-Boot `go` 的缓存行为**：没有查看源码；只能根据现象判断“不能依赖它自动刷缓存”。
- **SD 读取错误**：`fatload` 报告 4528 字节且 CRC 正确，基本排除；但没有做多次重复读校验。
- **CPU1 干扰**：demo 内核没有 SMP 启动代码，CPU1 状态未检查。

## 5. 解决办法与复现指南

### 5.1 已验证的成功配方

```text
fatload mmc 0:1 0x00100000 kernel.bin
md.l 0x00100000 4
crc32 0x00100000 0x11b0
dcache flush
icache flush
go 0x00100020
```

地址规则：

- `0x00100000`：`kernel.bin` 的加载基址，也是向量表 `vector_table` 的地址。
- `0x00100020`：`_start` 的地址，也是 `kernel.elf` 的 ELF entry。
- 不要把 `kernel.bin` 加载到 `0x00100020`；那会把整个镜像后移 0x20。
- `go 0x00100000` 会先执行向量表第一条 `b _start`；本次没有在刷缓存后重试这个地址。

### 5.2 预期结果

```text
4528 bytes read
00100000: ea000006 ea00033c ea00034d ea000340
crc32 ... ==> c1a8b40e

## Starting application at 0x00100020 ...
[boot] operating-systems-lab: minimal kernel
[boot] arch: armv7-a (Zynq-7010, Cortex-A9)
[boot] console: Cadence UART0 at 0xE0000000
[irq] GIC distributor 0xf8f01000, CPU interface 0xf8f00100
[irq] private timer interrupt 29 enabled
[mem] physical memory: 0x100000 - 0x20000000 (511 MiB)
[clock] private timer armed: 333333333 ticks
[boot] hello from the kernel
[clock] tick 1
[clock] tick 2
[clock] tick 3
```

### 5.3 复现清单

1. 构建：`make ARCH=arm bin`。
2. 记录 `kernel.bin` 的大小、CRC32、前四个字。
3. 确认 SD 卡设备，写入原厂镜像（会清卡）。
4. 把 `kernel.bin` 复制到 FAT 的 `BOOT` 分区，校验 SHA-256，卸载并弹出。
5. 插卡，跳线设为 SD 启动，UART 接 115200 8N1，上电。
6. 倒计时时按空格进入 `Zynq>`。
7. 执行 5.1 的命令。
8. 串口出现 `[boot] operating-systems-lab`、GIC、timer、`tick 1`～`tick 3` 即成功。
9. 退出：复位或断电；回原厂 Linux：重启后不按键。

### 5.4 后续验证步骤（待验证）

以下方案尚未实测成功，不能写成已解决：

1. **重复性**：连续启动 3 次，确认每次都能从 `0x00100020` 进入。
2. **隔离地址变量**：刷缓存后尝试 `go 0x00100000`，确认向量表路径是否也可用。
3. **隔离缓存变量**：只刷 `dcache`、只刷 `icache`、两者都不刷，分别测试，确认最小必要步骤。
4. **去掉 `md`/`crc32`**：确认这两个读操作是否影响结果（理论上不应）。
5. **`bootelf`**：把 `kernel.elf` 复制到卡上，尝试 `fatload` 到临时地址后 `bootelf`；U-Boot 字符串里有 `bootelf`，但没有实测。
6. **自动启动**：设置 `bootcmd` 或 `boot.scr`，包含缓存刷新；注意 `saveenv` 可能写 QSPI，需先确认存储位置和可恢复性。
7. **独立 BOOT.BIN**：用 Vitis/SDK 生成 `FSBL.elf`，写 `boot.bif`，用 `bootgen` 打包 FSBL + `kernel.elf`。Zynq BootROM 只认 FSBL，FSBL 不能省。
8. **JTAG 下载**：见 JTAG 实录；OpenOCD 通道已验证，但还没有下载本项目内核。
9. **看门狗**：检查 SWDT 状态，确认长时间运行不会复位。
10. **MMU/cache/DTB**：demo 内核尚未实现，后续需要补。

### 5.5 注意事项

- 写 `z7_lite_10.img` 会重建整张 SD 卡的分区并清除数据；操作前必须用 `lsblk` 核对设备，不能选到系统盘。
- 不要删除 DATA 盘上的原厂镜像、教材和 PDF；它们是恢复与对照的唯一来源。
- 串口自动化时注意 DTR/RTS 可能复位板子；不要在关键步骤之间反复开关串口。
- `go` 之后 U-Boot 不再控制 CPU；要退出只能复位或断电。
- `saveenv` 可能写 QSPI 或其他持久存储；前期调试建议手动执行，不要急着保存。
- UART 和 JTAG 是板上的两个不同 Type-C 接口；不要混插。

### 5.6 快速排障表

| 现象 | 优先检查 | 处理 |
| --- | --- | --- |
| 串口完全无数据 | 板子电源、UART/JTAG 口、115200、CH340 | 换 UART 口，确认供电，重插 CH340 |
| 看不到 U-Boot，直接进 Debian | 4 秒倒计时 | 登录后 `reboot`，连续按空格 |
| 抓到 U-Boot 后又复位 | 是否重新打开了串口 | 全程一个进程，`dtr=False`、`rts=False` |
| `md` 全是 `ffffffff` | 是否执行了 `fatload` | 重新 `fatload`，再 `md`/`crc32` |
| `crc32` 不匹配 | 卡上文件、加载地址、长度 | 重新复制 `kernel.bin`，确认 `0x11b0` |
| `go` 报 `undefined instruction` | 缓存是否刷新 | `dcache flush`、`icache flush`，`go 0x00100020` |
| `mmc 0:1` 读取失败 | SD 设备号 | `mmc list`，`fatls mmc 0:1` |
| 内核运行几秒后复位 | 看门狗/异常 | 抓完整串口日志，检查 SWDT 和异常帧 |

## 6. 当前状态与文件清单

### 6.1 构建产物（本次记录时）

| 文件 | 大小 | SHA-256 | 说明 |
| --- | --- | --- | --- |
| `build/arm/kernel.bin` | 4528 字节（0x11b0） | `66f48bd8cd829889471fe3ed860dcad4c39dac3ef60c502be0347bf7b32dd0e1` | 裸二进制，CRC32 `c1a8b40e` |
| `build/arm/kernel.elf` | 88888 字节 | `2c53dc757f0884b6c61282baa85449930002ebffde3c92446a34478c0b9a687d` | ELF entry `0x100020` |

### 6.2 项目路径

```text
宏内核根：/home/jerry/Projects/from-scratch/operating-systems-lab/mono
链接脚本：kernel/arch/arm/linker.ld
启动代码：kernel/arch/arm/boot.S
向量表：kernel/arch/arm/vectors.S
平台初始化：kernel/arch/arm/platform.c
UART：kernel/arch/arm/uart.c
时钟：kernel/arch/arm/clock.c
中断：kernel/arch/arm/irq.c
构建：Makefile（ARCH=arm bin）
本文：docs/Z7-Lite-SD卡经U-Boot启动内核实录.md
JTAG：docs/Z7-Lite-JTAG连接与调试实录.md
```

### 6.3 外部资料

```text
教材与镜像根：
/media/jerry/Data/PROJECTS/Z7-Lite_Z7010_2018.3/

7010 启动镜像：
01_微相Z7-lite ZYNQ开发板开箱检测/7010开箱测试images/z7_lite_10.img

原理图：
02_Z7-Lite硬件资料/Z7-LITE_Rev1_1.pdf

四册教程：
Part1_Z7_Lite系列教程之逻辑篇 V1.1.pdf
Part2_Z7-Lite系列教程之SDK篇 V1.1.pdf
Part3_Z7_Lite系列教程之HLS篇 V1.1.pdf
Part4_Z7-Lite系列教程之Linux系统篇 V1.1.pdf
```

### 6.4 状态表

| 项目 | 状态 |
| --- | --- |
| ARM 内核构建 | 已实测 |
| 复制到 SD 卡 | 已实测 |
| SD 启动到 U-Boot | 已实测 |
| U-Boot 加载 kernel.bin | 已实测 |
| 内核在实机运行 | 已实测（GIC、timer、PMM、tick 1-3） |
| 刷缓存后 `go 0x00100000` | 未验证 |
| 自动启动（bootcmd/boot.scr） | 未验证 |
| 独立 BOOT.BIN（FSBL 直接加载） | 未验证 |
| JTAG 下载内核 | 未验证 |
| MMU / D-cache / 设备树 | 未实现 |
| CPU1 / SMP | 未验证 |
| 看门狗 | 未检查 |

## 附录 A：关键日志

### A.1 U-Boot 启动

```text
U-Boot 2018.01 (Jul 10 2020 - 07:33:19 +0000) Xilinx Zynq ZC702
Model: Navigator Development Board
Board: Xilinx Zynq
Silicon: v3.1
DRAM:  ECC disabled 512 MiB
MMC:   mmc@e0100000: 0 (SD)
In:    serial@e0000000
Out:   serial@e0000000
Err:   serial@e0000000
Hit any key to stop autoboot:  4
Zynq>
```

### A.2 失败：`go 0x00100000`

```text
## Starting application at 0x00100000 ...
undefined instruction
pc : [<00100000>]  lr : [<1ff3dafc>]
Flags: nZCv  IRQs off  FIQs off  Mode SVC_32
Resetting CPU ...
```

### A.3 成功：`go 0x00100020`

```text
## Starting application at 0x00100020 ...
[boot] operating-systems-lab: minimal kernel
[boot] arch: armv7-a (Zynq-7010, Cortex-A9)
[boot] console: Cadence UART0 at 0xE0000000
[irq] GIC distributor 0xf8f01000, CPU interface 0xf8f00100
[irq] private timer interrupt 29 enabled
[mem] physical memory: 0x100000 - 0x20000000 (511 MiB)
[clock] private timer armed: 333333333 ticks
[boot] hello from the kernel
[clock] tick 1
[clock] tick 2
[clock] tick 3
```

### A.4 Debian 登录

```text
Debian GNU/Linux 8 zynq ttyPS0
zynq login:
Linux zynq 4.14.0-xilinx #13 SMP PREEMPT Fri Jul 10 07:32:55 UTC 2020 armv7l
```

## 附录 B：串口单进程操作要点

如果下次用脚本自动化，关键不是脚本多复杂，而是：

1. 整个流程只打开串口一次。
2. 打开后设置 `p.dtr = False`、`p.rts = False`。
3. 登录、`reboot`、发空格、等 `Zynq>`、`fatload`、`go` 都在同一个进程里完成。
4. 不要在抓到 `Zynq>` 后关闭串口再开新进程。
5. 串口设备优先用 `/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0`，避免 `ttyUSB0/ttyUSB1` 编号变化。
