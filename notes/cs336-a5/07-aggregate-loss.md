# 07 · 损失聚合（aggregate_loss_across_microbatch）

讲义 §4.2.3 / §5.1 ｜ 覆盖 2 个 Problem: `aggregate_loss_across_microbatch_sequence`(0.5 分) / `_constant`(0.5 分) ｜ adapter: `run_aggregate_loss_across_microbatch` ｜ 测试: `uv run pytest -k test_aggregate_loss_across_microbatch_sequence` / `_constant`

```text
Problem:
aggregate_loss_across_microbatch — 把逐 token 损失聚合成标量
两种归一化: sequence（标准 GRPO）/ constant（Dr.GRPO 等）

输入:
per_token_policy_gradient_loss: (B, S)
mask: (B, S)            response 位置为 1
loss_normalization: "sequence" | "constant"
normalization_constant: int | None      constant 模式必需

输出:
loss: 标量 tensor（必须能 backward）

参数:
无

公式:
sequence: 先每条序列内平均，再across序列平均
  loss = mean_b( Σ_t loss[b,t]·mask[b,t] / Σ_t mask[b,t] )
constant: 全部求和除以固定常数
  loss = Σ_{b,t} loss[b,t]·mask[b,t] / Z
  （Z 典型取 B·G·L_max = train_batch_size × 最大生成长度 512，
   训练全程固定）

步骤:
1. masked = per_token_loss * mask
2. sequence 分支: per_seq = masked.sum(-1) / mask.sum(-1)；
   return per_seq.mean()
3. constant 分支: return masked.sum() / normalization_constant
4. 不支持的输入 raise NotImplementedError

需要查的 API:
- Tensor.sum(dim=-1) / mean()
- 无其他新 API——重点在梯度累积语义（见下）

边界情况:
- 与梯度累积的配合（这是本卡真正的坑）:
  sequence 归一化: 微批 loss 是"微批内均值"，
    累积时要乘 (microbatch_size / full_batch_size) 才等价于全批梯度
  constant 归一化: 每个微批都除同一个 Z，微批 loss 天然可直接相加，
    "不要"再乘任何比例
- mask.sum(-1)=0 的序列（空 response）会除零——上游应过滤或特判
- sequence vs constant 的语义差（考点）: sequence 把长序列的每个 token
  权重稀释（长回复被降权），constant 对所有 token 一视同仁
- 返回值不要 detach——训练要 backward

测试:
- uv run pytest -k test_aggregate_loss_across_microbatch_sequence
- uv run pytest -k test_aggregate_loss_across_microbatch_constant
- 手算: loss=[[1,1],[2,0]], mask=[[1,1],[1,0]]
  sequence -> mean(1, 2) = 1.5；constant(Z=4) -> 4/4 = 1.0
```
