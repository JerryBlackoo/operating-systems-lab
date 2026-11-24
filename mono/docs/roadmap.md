# 教材章节 ↔ 仓库目录 ↔ Goal 路线图

本文对应 G0：冻结基线，并把王道《2027 操作系统考研复习指导》第 1–5 章、汤小丹《计算机操作系统》第 1–9 章映射到本仓库的目录、Goal 和验收命令。

## 基线

- 基线日期：2026-11-19
- 基线能力：启动、UART、异常/时钟、物理页分配；无进程、调度、系统调用、虚拟内存、文件系统。
- 构建：clang + ld.lld；RISC-V 64 QEMU virt；ARM Cortex-A9 / Zynq-7010。
- 基线日志：
  - RISC-V：`/tmp/oslab-g0-riscv.log`
  - ARM：`/tmp/oslab-g0-arm.log`

两架构均出现：

```text
[boot] hello from the kernel
[clock] tick 1
```

## 章节映射

| 教材范围 | Goal | 主要目录 | 验收命令 |
|---|---|---|---|
| 王道 1.1–1.6；汤 1.1–1.5 | G0 | `docs/` | `make && timeout 8s make run`；`make ARCH=arm && timeout 12s make ARCH=arm run` |
| 王道 1.5–1.6；汤 1.3–1.5 | G1 | `shared/abi/`、`kernel/syscall/`、`userland/hello/` | `make && make run`，出现 `hello from user` |
| 王道 2.1；汤 2.1–2.3 | G2 | `kernel/proc/` | `make && make run`，出现 `[proc]` 列表 |
| 王道 2.2；汤第 4 章 | G3 | `kernel/proc/sched.c`、`kernel/arch/*/switch.S` | 两个 pid 的 `tick` 日志交错 |
| 王道 2.3；汤第 3 章 | G4 | `kernel/sync/`、`kernel/tests/` | `[sync] prodcons ok count=1000` |
| 王道 2.4；汤第 5 章 | G5 | `kernel/sync/banker.c` | `[banker] safe seq: ...` |
| 王道 3.1；汤第 6 章 | G6 | `kernel/mm/`、`kernel/arch/*/vm.*` | `[vm] selftest map/unmap ok` |
| 王道 3.1–3.2；汤第 7 章前半 | G7 | `kernel/mm/uvm.c`、`kernel/proc/exec.c` | 两个用户进程同虚拟地址不同物理页帧 |
| 王道 3.2；汤第 7 章后半 | G8 | `kernel/mm/trap_vm.c`、`kernel/mm/swap.c` | `[vm] faults=... evictions=...` |
| 王道 4.1、4.3；汤第 8 章前半 | G9 | `kernel/fs/bio.c`、`kernel/fs/ramdisk.c` | `[bio] pattern rw ok; hit=... miss=...` |
| 王道 4.2–4.3；汤第 8 章 | G10 | `kernel/fs/`、`tools/mkfs.py` | 用户态 `cat /motd` 或内置读测试 |
| 王道 5.1–5.2；汤第 9 章 | G11 | `kernel/dev/`、`kernel/drivers/` | 控制台正常；`/dev/null`、`/dev/zero` 自测 |
| 王道 5.3；汤第 9 章 | G12 | `kernel/fs/disk_sched.c` | `[disk] scan order: ...` |
| 王道 1.2 等；综合 | G13 | `userland/init.c`、`userland/sh.c` | `help`、`cat /motd`、利用率演示 |
| 板级手册 | G14 | `docs/`、`tools/deploy-kernel-sd.sh` | UART 出现 `[boot] hello from the kernel`，或文档签收 |

## 依赖波次

```text
G0 → G1 → G2 → G3 → G13
        ├→ G6 → G7 → G8 → G13
        ├→ G9 → G10 → G13
        └→ G11 → G12
G4 → G5
G14 依赖 G1（可选）
```

## 本轮验收记录（2026-11-19）

| 范围 | 日志 | 关键行 |
|---|---|---|
| G0 基线 | `/tmp/oslab-g0-riscv.log`、`/tmp/oslab-g0-arm.log` | `[boot] hello from the kernel`、`[clock] tick 1` |
| G1–G13 RISC-V | `/tmp/oslab-final-riscv.log` | `hello from user`、`prodcons ok count=1000`、`faults=144 evictions=48`、`sh>` |
| G1–G13 ARM | `/tmp/oslab-final-arm.log` | `[vm] selftest map/unmap ok (ARM 4KiB)`、`[sched] demo ready` |
| G14 | `docs/evidence/z7-boot-20261119.txt` | 基线内核实板日志；当前多 Goal 内核尚未重新实板验证 |

G14 状态：文档、启动片段、部署脚本 dry-run 已完成；实板启动当前内核待用户签收。

## 说明

- 每个 Goal 的详细验收以 `docs/goals/G*.md` 为准。
- `docs/design/G*.md` 记录教材节号、实现取舍和与 Linux 命名的差异。
- 基线日志是“改动前”的证据；后续 Goal 完成后再追加对应日志，不回填未验证能力。
- ARM 已跑通 `hello/fork/exec/shell`，4 KiB L2 小页已通过 `vm_selftest`；ARM 缺页、完整 IRQ 抢占、D-cache 一致性仍未完成，不能写成已验证。
