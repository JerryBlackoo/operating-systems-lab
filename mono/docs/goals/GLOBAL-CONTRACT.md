# 全局合同（所有 Trae Goal 共用）

## 仓库与基线

- **路径**：`/home/jerry/Projects/from-scratch/operating-systems-lab/mono`（仓库根：`.../operating-systems-lab`）
- **权威现状**：以根目录 `README.md`「当前状态 / 尚未实现」为准；不得把未合并能力写进文档或验收脚本。
- **构建**：`clang` + `ld.lld` / `rust-lld`；**禁止**为完成 Goal 引入 GCC 交叉链或大型第三方库（除非 Goal 正文明确允许且只限该 Goal）。

## 双架构

| 架构 | 主验收 | 命令 |
|------|--------|------|
| RISC-V 64 QEMU virt | **必须** 行为验收 | `make && make run` |
| ARM Zynq-7010 / QEMU | **必须** 至少编译通过 | `make ARCH=arm && make ARCH=arm run` |

架构相关代码放在 `mono/kernel/arch/{riscv,arm}/`（自 `mono/` 为 cwd 时即 `kernel/arch/`）；通用逻辑放在 `kernel/` 子目录，通过 `arch.h` 或等价抽象访问硬件。

## 教材纪律

1. 每个 Goal 在 `docs/design/<goal-id>.md` 记录：
   - 王道**节号** + 汤小丹**章/节** + 本仓库**实现取舍**（删减了什么、为何可接受）。
2. 术语优先与两教材一致（进程/线程/管程/页表项等）；若与 Linux 命名不同，在 design 文档对照表说明。
3. **不得**把 Linux 5.x 子系统整棵移植冒充「按教材实现」；只实现教学 OS 最小子集。

## 交付物

- 代码（限定 Goal 所列目录）
- `docs/design/<goal-id>.md`
- 可重复验收：终端命令 + 期望日志片段（写入各 Goal 文件）

## 默认禁止（除非 Goal 放开）

- 多核 SMP、容器、网络协议栈完整实现
- 完整 ext4 / btrfs / 桌面级 shell
- 修改 `AGENTS.md` 或无关科目仓库
- 提交凭据、私钥、本机绝对路径以外的秘密

## 冲突裁决

1. 本仓库 `docs/design/` 中**已合并**的裁决优先于未合并 PR。
2. 两教材冲突时：在 design 文档写明选用哪本的说法，另一本记为「考点差异，未实现」。
3. 与 `README.md` 基线冲突时：**先更新 README「当前状态」再宣称完成**。

## 每个 Agent 任务结束三行（复制到 Trae 备注）

```text
必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。
冲突时：以 docs/design/ 裁决为准并引用教材节号。
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。
```