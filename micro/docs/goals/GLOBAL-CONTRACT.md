# 全局合同（所有 micro Goal 共用）

## 仓库与基线

- **根目录**：`/home/jerry/Projects/from-scratch/operating-systems-lab/micro`（仓库根：`.../operating-systems-lab`）
- **权威现状**：以同目录 [`README.md`](README.md) 的「基线事实」为准；任何 Goal 完成后必须同步更新该表，不许只改代码不改状态。
- **构建**：`clang` + `ld.lld` / `rust-lld`；QEMU `qemu-system-riscv64`（本机 8.2.2）。**禁止**引入 GCC 交叉链、CMake、第三方运行时或任何需要网络的构建依赖。
- **目标平台**：`-machine virt -m 128M -smp 1`，`rv64imac_zicsr_zifencei` / `lp64` / `medany`，OpenSBI `-bios default`。改平台参数必须同步 `kernel/arch/riscv/platform.h` 的注释来源与 `docs/evidence/` 记录。

## 与 `mono/` 的边界

- **不修改 `mono/` 下任何文件**。它是宏内核教学树，有独立的 G0–G14 验收记录；混改会让其证据失效。
- 可复用的只有思路与教材节号，不复制 `mono/` 源码。微内核的抽象（cap/endpoint/AS 分离）与宏内核（PCB 一把抓）是两套模型。
- 根 `README.md` 的「微内核」一节可以更新状态，但不得把 `mono/` 的能力写成 `micro/` 的。

## 两个层次的验收

每个模块同时有两层，缺一层不算完成：

| 层 | 命令形态 | 证明什么 |
| --- | --- | --- |
| **host 自测** | `make -C micro/kernel/<mod> test` | 状态机与不变量在宿主上可判定 |
| **QEMU 镜像** | `make -C micro run BOOT_MODULES=<mod>` | 同一套代码在真实特权级/trap/设备上成立 |

系统级 Goal（M1–M7）额外要求：`make -C micro run`（默认镜像）本身也要通过，且**不能靠删掉模块 demo 来换通过**。

## 证据纪律（本树最重要的一条）

1. 每个 Goal 在 `micro/docs/evidence/` 留一条记录：`<goal-id>-<日期>.log`，含**完整命令**、**关键日志行**、**退出码**。
2. **没有 log 不许写"已实现"**。README、goals 状态表、commit message 三处口径必须一致。
3. 日志里出现的每一行都要能解释来源：内核打印的、用户程序经 ecall 让内核代打的、还是宿主伪造的——三者不许混淆。
4. `PASS` 只证明它前面那段断言，不证明整个子系统。写 README 时必须指明 `PASS` 的**边界**（现有各模块 README 的「已知边界」节是本树的标准写法，继续沿用）。
5. 失败的检查要保留，写进状态表。**不因修不好而从验收命令里删掉**。

## 替身（stand-in）规则

本树已有大量替身：内存盘、链路替身、控制器替身、provider 句柄替身、fixture token。规则：

- 替身**允许**留在 host 自测和单模块 demo 里——它们证明模型本身。
- 真实路径上**不允许**存在替身。每个 Goal 必须能指出"哪一行把替身换成了真实实现"，并在 README 里把该条从「已知边界」移出。
- 新增替身必须在文件头注明 `/* 替身：<替代什么>；真实实现在 <goal-id> */`。

## 不得为通过验收而改断言

这是本树已经踩过的坑（M0 的 `objects` 自测）：

1. 先证明是**测试**错了还是**实现**错了，把推理写进 commit message。
2. 测试错了改测试，理由必须包含"哪条 API 契约被误解"。
3. 实现错了改实现，不许放宽断言、不许删用例、不许把 `assert` 改成 `printf`。

## 提交范围

- 一次只碰一个模块目录 + 必要的接线文件（`micro/Makefile`、`shared/modules.mk`、`shared/demo.c`、`kernel/boot/bootstrap.c`）。
- 新增跨模块 API 必须同时写进对应 README 的「接缝」或「API」节，否则视为未完成。
- 保留 `BOOT_MODULES=<单模块>` 能力。为"整合"而拆模块 demo 是明确禁止项。
- 不顺手重构无关模块，不改命名风格，不动 `build/` 产物。

## 冲突裁决

1. 已合并的模块 README「不变量」优先于任何 Goal 正文。想改不变量，先改 README 并说明为什么旧不变量是错的。
2. 教材与实现冲突时：在 `docs/design/<goal-id>.md` 写明采用哪本、另一本记为"考点差异，未实现"。
3. 与 `micro/docs/goals/README.md`「基线事实」冲突时：**先更新基线表**再宣称完成。

## 每个 Agent 任务结束三行（复制到交付说明）

```text
必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。
冲突时：以模块 README 的不变量为准并引用教材节号。
完成后：只提交本 Goal 范围 diff，附 docs/evidence/<goal-id>-<日期>.log。
```
