# 微内核 Goals：从模块模型到可运行的微内核系统

本目录是 `micro/` 的目标正文，与 [`mono/docs/goals/`](../../../mono/docs/goals/README.md)（宏内核 G0–G14）并列，**互不替代**：

| 树 | 类型 | Goal 前缀 | 基线 |
| --- | --- | --- | --- |
| `mono/` | 宏内核，RISC-V + ARM | G0–G14 | 已完成教学子集 |
| `micro/` | 微内核，RISC-V | M0–M7（本目录） | M1–M7 主路径已跑通 |

**微内核根目录**：`/home/jerry/Projects/from-scratch/operating-systems-lab/micro`

**全局合同**：[`GLOBAL-CONTRACT.md`](GLOBAL-CONTRACT.md)

## 基线事实（2026-11-25 实测）

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| M7 一条命令 | `make -C micro demo` 或 `./tools/demo.sh` | PASS：help/cat/motd/ps/echo/poweroff |
| M5 判别 | `./tools/m5_discriminate.sh` | PASS |
| M6 判别 | `./tools/m6_discriminate.sh` | PASS（双进程同 VA、deny rights） |
| M0 自检 | `make -C micro check` | 首步 `tools/verify_capability_wiring.sh`；host + shared + `BOOT_MODULES=all` |
| 默认常驻 | `make -C micro run BOOT_MODULES=capability QEMU_TIMEOUT=45s` | `micro>` 提示符、无 `[kmain] shutting down`（timeout 结束） |
| 接线约束 | capability `Makefile` `C_SRCS` | 必须 **`kernel_init.c` + `service_boot.c`**；**勿**默认链 `*_m6.c`。init 必须 **`init_main.o`**。见 `docs/evidence/M7-2026-11-25-makefile-m6-fix.log` |

**混合启动（当前权威）**：内核 spawn `sm/pm/vm/heaptest/heaptest2/init`；init `plan=3` 经 PM 拉 `fs/console/shell`。PM-only plan=6 为实验路径（`init_main_m6.c` / `kernel_init_m6.c`），未作默认。

**当前准确定性**：端到端 objective（`docs/evidence/OBJECTIVE-2026-11-25-audit.log`）与 M0–M7 当日签收已完成；实验路径 PM-only `*_m6.*` 非默认、非 objective 要件。

## 依赖与波次

| 波次 | 可并行 Goal | 说明 |
| --- | --- | --- |
| 1 | M0 | `make check`、证据目录 |
| 2 | M1 | 帧池、内核常驻、idle |
| 3 | M2、M4 | initrd/ELF 与控制台 |
| 4 | M3 | init→SM/PM→服务 |
| 5 | M5、M6 | 存储与 VM |
| 6 | M7 | `demo.sh` 综合 |

## Goal 索引

| ID | 文件 | 简述 |
| --- | --- | --- |
| M0 | [M0-baseline-check.md](M0-baseline-check.md) | `make check` |
| M1 | [M1-kernel-resident.md](M1-kernel-resident.md) | 常驻内核 |
| M2 | [M2-initrd-elf.md](M2-initrd-elf.md) | initrd + U-mode ELF |
| M3 | [M3-service-ipc.md](M3-service-ipc.md) | SM/PM/IPC |
| M4 | [M4-console-irq.md](M4-console-irq.md) | 控制台中断 |
| M5 | [M5-block-ext4.md](M5-block-ext4.md) | virtio→ext4 |
| M6 | [M6-vm-server.md](M6-vm-server.md) | pager + VM 服务 |
| M7 | [M7-integration.md](M7-integration.md) | shell + demo.sh |

证据目录：`docs/evidence/`（按 `<goal-id>-<date>.log` 命名）。

**2026-11-25 签收**：[`M0-M7-2026-11-25-signoff.log`](../evidence/M0-M7-2026-11-25-signoff.log)、[`GOAL-2026-11-25-completion-audit.log`](../evidence/GOAL-2026-11-25-completion-audit.log)。  
**2026-11-25 回归**：console 修复 → [`M7-2026-11-25-console-fix.log`](../evidence/M7-2026-11-25-console-fix.log)；常驻 → [`M1-2026-11-25-resident-run.log`](../evidence/M1-2026-11-25-resident-run.log)。