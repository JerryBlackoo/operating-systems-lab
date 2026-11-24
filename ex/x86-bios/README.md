# x86 BIOS 启动实验

这个实验把王道里的传统启动路径写成能执行的代码。SeaBIOS 读 MBR，MBR 找活动分区并读 PBR，PBR 读内核，内核入口切换到 32 位保护模式后调用 C 函数 `kmain`。

## 编译运行

从 `ex/x86-bios` 执行，使用 GNU Make、GNU binutils、支持 `-m32` 的 GCC 和 QEMU。

```sh
make
make run
```

```text
MBR: stage 1
PBR: stage 2
kmain: 32-bit protected mode
```

三个阶段向 QEMU 的 `0xe9` 调试端口写字符。内核打印完执行 `hlt`，用 Ctrl+C 结束 QEMU。Makefile 把引导代码写入 `../../mono/build/x86-bios/boot.img`，运行时把该文件用作 QEMU 的 IDE 磁盘。

## 磁盘位置和内存位置

| 镜像中的位置 | 谁加载 | 内存地址和入口 |
|---|---|---|
| LBA 0，MBR | SeaBIOS | `0x7c00`，MBR 随即把自己搬到 `0x0600` |
| LBA 2048，活动分区首扇区 | MBR | `0x7c00`，覆盖旧的 MBR 副本 |
| LBA 2049，活动分区第二扇区 | PBR | `0x1000`，这里是内核的汇编入口 `_start` |

1. MBR 扫描 `0x1be` 起的四个 16 字节分区表项。第 0 字节为 `0x80` 时表示活动分区，第 8 字节起的 4 字节是起始 LBA。代码用 BIOS `INT 13h / AH=42h` 加载 PBR 并检查签名，然后把启动磁盘号 `DL` 和分区表项指针 `DS:SI` 交给 PBR。[mbr.S](mbr.S)
2. PBR 从表项算出 `起始 LBA + 1`，用同一个 BIOS 接口读一个扇区到 `0000:1000`，再远跳转到那里。这个分区使用裸扇区布局，容量和放置位置由 Makefile 决定。[pbr.S](pbr.S) · [Makefile](Makefile)
3. 内核汇编入口安装 GDT、置位 `CR0.PE`、远跳转到 32 位代码段，设置栈并清零 `.bss` 后调用 C 函数 `kmain`。链接脚本把内核入口放在 `0x1000`，并检查镜像能放进一个扇区。[kernel-entry.S](kernel-entry.S) · [kernel.ld](kernel.ld) · [kernel.c](kernel.c)

```sh
out=../../mono/build/x86-bios
img="$out/boot.img"
fdisk -l "$img"
xxd -g 1 -l 16 "$img"
xxd -g 1 -s 0x1be -l 66 "$img"
xxd -g 1 -s $((2048 * 512)) -l 16 "$img"
xxd -g 1 -s $((2048 * 512 + 510)) -l 2 "$img"
xxd -g 1 -s $((2049 * 512)) -l 16 "$img"
readelf -h "$out/kernel.elf"
objdump -D -b binary -m i8086 -M intel --adjust-vma=0x7c00 "$out/pbr.bin" | head -n 20
```

MBR 表项起始 LBA 的字节是 `00 08 00 00`，小端值为 `0x800`，也就是 2048。乘以每扇区 512 字节得到镜像的字节偏移 `0x00100000`。这个偏移恰好和 Z7-Lite 的 DDR 加载地址数值相同。镜像偏移用 `xxd -s` 查看，DDR 地址用 U-Boot 的 `md.l` 查看。

## 在 CPU 复位和每次交接处断下

终端一运行 `make debug`。终端二在同一目录运行 `gdb -q -nx ../../mono/build/x86-bios/kernel.elf`，随后执行以下 GDB 命令。

```gdb
target remote 127.0.0.1:1234
monitor info registers
monitor xp /16bx 0xfffffff0
hbreak *0x7c00
continue
x/8bx 0x7c00
continue
x/8bx 0x7c00
info registers cs eip edx esi
disable 1
hbreak *0x1000
continue
x/8bx 0x1000
hbreak kmain
continue
monitor info registers
```

- 复位时 QEMU 显示 `EIP=0000fff0` 和 `CS =f000 ffff0000 ...`。CS 的隐藏基址加 EIP 得到 `0xfffffff0`，前五个字节 `ea 5b e0 00 f0` 在这个 SeaBIOS 构建中解码为 `jmp 0xf000:0xe05b`。
- 第一次停在 `0x7c00`，起始字节是 `fa 31 c0 8e`，对应 MBR。第二次开头变成 `66 8b 44 08`，对应 PBR，且 `DL=0x80`、`SI=0x07be`。分区表随 MBR 搬到了 `0x0600 + 0x1be`。
- `0x1000` 处的字节与 `kernel.bin` 对应。停在 `kmain` 时，可看到 `CS=0x08` 和 `CR0.PE=1`。输入 `detach` 让其运行并打印，终端一按 Ctrl+C 结束。

实模式下用 GDB 读原始字节，用 `objdump -m i8086` 解码。这里的 GDB 15.1 连上 QEMU 后，`x/i` 曾将 16 位指令按 32 位解释。

```sh
tail -c 16 /usr/share/seabios/bios-256k.bin | xxd -g 1
objdump -D -b binary -m i8086 -M intel --adjust-vma=0xfffc0000 \
  --start-address=0xfffffff0 --stop-address=0xfffffff5 /usr/share/seabios/bios-256k.bin
```

## 复现一次失败并定位停在谁手里

先运行 `make` 更新正常镜像，再复制一份，单独清除 PBR 签名。MBR 仍能找到活动分区、读到该扇区，然后在签名检查处停住。

```sh
out=../../mono/build/x86-bios
cp "$out/boot.img" "$out/bad-pbr.img"
printf '\000\000' | dd of="$out/bad-pbr.img" bs=1 seek=$((2048 * 512 + 510)) conv=notrunc status=none
make run IMAGE="$out/bad-pbr.img"
```

输出停在 `MBR: bad PBR signature`。结束 QEMU 后再跑 `make run`，用正常镜像验证三段输出。

## 接回原来的实验

- Z7-Lite 上用 OpenOCD `halt` 和 `reg pc` 看暂停时的执行位置。这里用 `-S` 把停点提前到复位，用 `monitor info registers` 同时看 EIP 和 CS 基址。[JTAG 实录](../../mono/docs/Z7-Lite-JTAG连接与调试实录.md)
- U-Boot 的 `fatload` 把 `kernel.bin` 读到 DDR `0x00100000`。随后用 `md.l` 和 `crc32` 核对内存内容，刷新缓存后 `go 0x00100020` 进入 `_start`。这里的 PBR 用 `INT 13h` 搬数据，用远跳转交接，用 GDB 核对目标内存。[SD 启动实录](../../mono/docs/Z7-Lite-SD卡经U-Boot启动内核实录.md)
- 之前的 RISC-V QEMU 用 `-kernel` 加载 ELF，OpenSBI 进入 S-mode 并交给 `0x80200000` 的 `_start`。这里 QEMU 提供磁盘模型，MBR 和 PBR 的读盘、跳转由自己的汇编完成。[RISC-V 启动记录](../../mono/bootloader/README.md)
