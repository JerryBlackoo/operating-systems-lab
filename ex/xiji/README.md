# 希冀（xiji）实验机制 — QEMU 真实硬件 + 算法双轨验收

本目录把 [`ex/xiji/*.md`](.) 中的课内实验落成可运行代码：**希冀笔记里的队列/分页/设备表算法**在 `src/` 实现；**QEMU PC 实机路径**在 `qemu/hw/` 用端口与中断验证同一套实验编号。

## 一键验收（推荐）

```sh
cd /home/jerry/Projects/from-scratch/operating-systems-lab/ex/xiji
make verify    # = make test + QEMU + 检查 build/traces/qemu.stdout
```

| 命令 | 作用 |
|------|------|
| `make test` | 宿主 gcc，跑 `exp01`–`exp07` 算法断言 |
| `make run` | QEMU TCG，硬件自检 + 算法（退出码 33） |
| `make verify` | 上述全部 + `verify.py` 关键字门禁 |

QEMU 日志：`build/traces/qemu.stdout`

## QEMU 用到的真实硬件（按实验）

| 实验 | 算法 (`src/`) | 硬件路径 (`qemu/hw/`) |
|------|---------------|------------------------|
| lab1 | — | **INT 13h** 读盘（stage2）、**0xe9 debugcon**、**0xf4 isa-debug-exit**、**CMOS 0x70/0x71** |
| exp01–02 | block/wake、FCFS/SJF/RR | **8259 PIC** + **8254 PIT** IRQ0 计数（`hlt` 等 tick） |
| exp03 | 分区 fit/free | 物理 RAM 读写校验（宿主页映射下的真实 store） |
| exp04 | 段式地址变换 | 恒等映射下的物理字节写回 |
| exp05 | FIFO/LRU/OPT 软件模拟 | **CR0.PG / CR3** 开启分页，**#PF** 按需建页（`0xe` 向量） |
| exp06 | 设备等待队列模型 | **IDE 主通道 0x1F0–0x1F7** 状态/忙位 |
| exp07 | 树形 FS 逻辑 | **ATA PIO LBA28** 写/读 **LBA 120**（`boot.img` 扩至 128 扇区） |

引导链：`x86-boot/boot.S` → `qemu/load-kernel.S` → `qemu/kernel-entry.S`（栈 `0xfc00`）→ `kmain`。

## 目录结构

```text
ex/xiji/
  src/              # 希冀算法（宿主 + QEMU 共用）
  qemu/
    hw/             # PIC/PIT/IDT/#PF/ATA/CMOS + hw_verify.c
    kernel.c
  verify.py         # 自动化门禁
  host/runner.c     # make test
  *.md              # 希冀笔记摘录
```

## 与课内 `*hard.c` 的关系

希冀平台框架仍用 `*hard.c` 填空提交；`src/` 提供可对号入座的标准逻辑。QEMU 轨证明这些机制可与 PC 设备语义对齐，不替代 GXemul/MIPS 课环境。

## 单独运行 QEMU

```sh
make run
make run QEMU=qemu-system-x86_64 ACCEL=kvm CPU_MODEL=host
```