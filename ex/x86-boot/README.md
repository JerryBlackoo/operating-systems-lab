# x86 两级 bootloader demo

对应王道第 1 章「操作系统引导过程」④～⑧ 步的最小可运行版本。
BIOS/UEFI 和 GRUB 的部分不在这里——那些不在内核树里。

## 文件

| 文件 | 作用 | 加载地址 |
| --- | --- | --- |
| `boot.S` | stage1，MBR 引导扇区，正好 512 字节 | `0x0000:0x7C00` |
| `stage2.S` | stage2，被 stage1 用 int 13h 读进来 | `0x0000:0x7E00` |
| `boot.img` | 前 512B + 后 512B 拼成的裸盘镜像 | — |

## 构建与运行

```sh
make            # 生成 boot.img，并打印两个 0x55AA 的位置
make run        # QEMU，0xE9 端口输出到终端
```

期望输出：

```text
stage1: hello from MBR (0x7C00)

stage2: hello, CS:IP=0000:7e2b DS=0000

stage2: control transfer ok, halting
```

## 内存布局

```text
0x00000  IVT（实模式中断向量表，BIOS 建）
0x00400  BDA
...
0x07C00  stage1（512B，末尾 0x55AA）
0x07E00  stage2（512B）
0x7FFFF  栈从这里向下长
```

## 三条硬约定

1. **0x7C00**：BIOS 读 MBR 的目标地址，写死在固件里。
2. **0xAA55**：MBR 最后两字节，缺了就不被当作可引导盘。
3. **far jump**：stage1 用 `EA offset:segment` 把 CS:IP 换成 `0000:7E00`，
   这是教材「控制权交接」在指令级的全部含义。

## 踩过的坑

`put_hex4` 第一版用 `%al` 当临时寄存器传字符，结果覆盖了 `%ax` 高位，
打印出假的 `CS=0003`。原因是 **BIOS 的 int 10h / int 13h 不保证保留寄存器**。
改成把值放在内存变量里（`hexval` / `hexcnt`），因为 int 10h 会改 ax/bx/cx。

实模式下写代码，凡是跨 BIOS 中断还活着的值，都必须放内存或显式 push/pop。

## 下一步

1. stage2 里建 GDT、置 `CR0.PE`、far jump 到 32 位代码段。
2. 打开 A20（`0x92` 端口或键盘控制器）。
3. 用 int 13h AH=42h 继续读更多扇区，把内核读到 `0x100000`。
4. 定义自己的 `boot_params` 结构，对照 Linux 的 `Documentation/x86/boot.rst`。
