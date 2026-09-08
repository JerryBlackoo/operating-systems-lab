# 13 · 交叉熵损失（cross_entropy）

讲义 §4.1 ｜ 1 分 ｜ adapter: `adapters.run_cross_entropy` ｜ 测试: `uv run pytest -k test_cross_entropy`

```text
Problem:
cross_entropy — 由 logits 和目标 ID 计算负对数似然（纯函数）

输入:
logits o: (..., vocab_size)   模型输出，未归一化
targets x: (...)              整数类别 ID（不是 one-hot），shape 与 batch 维一致

输出:
标量 —— 所有 batch 维上损失的平均值

参数:
无

公式:
ℓ = −log softmax(o)[target]
  = −o[target] + log Σₐ exp(oₐ)
数值稳定 + log/exp 抵消（m = max(o)）:
ℓ = −(o[target] − m) + log Σₐ exp(oₐ − m)

步骤:
1. m = logits.max(dim=-1, keepdim=True).values；shifted = logits − m
2. log_sum_exp = shifted.exp().sum(dim=-1).log()     # 或 torch.logsumexp
3. target_logit = 沿最后一维取出 targets 对应的 logit
   （gather(-1, targets.unsqueeze(-1)).squeeze(-1) 或 flatten + arange 索引）
4. loss_per_pos = log_sum_exp − target_logit
5. return loss_per_pos.mean()

需要查的 API:
- Tensor.gather(dim=-1, index=...)（index 要 unsqueeze 到同维数）
- torch.logsumexp(x, dim)
- Tensor.max(dim, keepdim).values
- unsqueeze / squeeze

边界情况:
- 千万不要先算 softmax 再取 log —— 小概率会下溢成 0，log(0)=-inf；
  要在公式层面把 log 和 exp 抵消
- 减 max 防上溢: exp(1000)=inf
- 任意 batch 维（batch、seq 都在前面），最后对全部平均
- targets 是整数索引，dtype 为 long
- 困惑度 perplexity = exp(平均损失)（写实验时会用）

测试:
- 输出是否为标量？
- 与 torch.nn.functional.cross_entropy(logits, targets) 数值一致？
- 大 logits（如 +1000）不出 NaN/inf？
- 手算: o=[0,0], target=0 -> ℓ = log 2 ≈ 0.6931
- uv run pytest -k test_cross_entropy
```
