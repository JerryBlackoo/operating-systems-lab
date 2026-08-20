# CS336 Assignment 3 (scaling) — 编程题速学卡片

来源: CS336 Spring 2026, Assignment 3: Scaling Laws (v26.0.5)。
配套仓库: [stanford-cs336/assignment3-scaling](https://github.com/stanford-cs336/assignment3-scaling)。

## 全局背景

- 场景：给定 48 B200 小时的训练预算，预测能取得最低验证 loss 的模型大小与超参。
- 不自己训练——通过课程训练 API 提交实验（另有 12 B200 小时的拟合预算，硬上限）。
- 核心方法：Chinchilla 的 IsoFLOPs 法。两个关键公式：\(C = 6ND\)、非嵌入参数 \(N \approx 12 \cdot n_{layer} \cdot d_{model}^2\)。
- 本作业只有 2 个 Problem，全部是编程/实验题，其中 leaderboard 题占 50 分。

## 编程题卡片索引（2 张）

| # | 卡片 | Problem | 分值 | 讲义 | 测试 |
|---|------|---------|------|------|------|
| 01 | [chinchilla_isoflops](01-chinchilla-isoflops.md) | 用给定数据复现 IsoFLOPs 拟合 | 5 | §2.1 | 无 pytest（图 + 预测值） |
| 02 | [scaling_laws](02-scaling-laws-api.md) | 训练 API 实验设计 + 拟合 + 最终预测 | 50 | §3 | 无 pytest（writeup + `/final_submission`） |

做题顺序：先用 01 的合成数据把拟合管线调通（分组取最小、log-log 幂律、外推、画图），再原样迁移到 02 的真实 API 数据上——02 的难点不在代码，在 12 小时预算内的实验设计。
