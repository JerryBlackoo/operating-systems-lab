# [G0] 基线冻结与教材映射表

**依赖**：无  
**建议 Agent**：A0（文档 / 集成）

## 背景

仓库已有最小内核：启动、UART、异常/时钟、物理页分配器。后续 Goal 需要统一的章节路线图与可重复验收脚本。

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 1.1 操作系统基本概念 | 1.1 操作系统的目标与作用 |
| 1.2 发展历程 | 1.2 操作系统的发展过程 |
| 1.3–1.6 体系结构、运行机制 | 1.3–1.5 基本概念、体系结构 |

## 交付

- `docs/roadmap.md`：第 1–5 章 ↔ 本仓库目录 ↔ Goal ID ↔ 验收命令
- `docs/trae-goals/README.md` 与本文档目录一致（若需增补链接）
- `tools/check-env.sh` 若缺项则补注释，**不改**内核逻辑

## 验收

```bash
cd /home/jerry/Projects/from-scratch/operating-systems-lab/mono
make && timeout 8s make run 2>&1 | tee /tmp/oslab-g0-riscv.log
make ARCH=arm && timeout 12s make ARCH=arm run 2>&1 | tee /tmp/oslab-g0-arm.log
```

期望日志包含（与 `README.md` 一致）：

- `[boot] hello from the kernel`
- `[clock] tick 1`（至少 1 次；ARM 在 QEMU 下间隔可能较长）

在 `docs/roadmap.md` 末尾附两段日志摘录路径。

## 非目标

- 实现新内核功能
- 实机 Z7（见 G14）

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。