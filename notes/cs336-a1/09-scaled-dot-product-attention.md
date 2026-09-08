# 09 · 缩放点积注意力（scaled_dot_product_attention）

讲义 §3.4.4 ｜ 5 分 ｜ adapter: `adapters.run_scaled_dot_product_attention` ｜ 测试: `uv run pytest -k test_scaled_dot_product_attention` 和 `-k test_4d_scaled_dot_product_attention`

```text
Problem:
scaled_dot_product_attention — Attention(Q,K,V) 纯函数

输入:
Q: (batch_size, ..., n, d_k)      n 个 query
K: (batch_size, ..., m, d_k)      m 个 key
V: (batch_size, ..., m, d_v)
mask: (seq_len, seq_len) bool，可选；True = 允许注意，False = 屏蔽

输出:
(batch_size, ..., n, d_v)

参数:
无（Q/K/V 是输入，不是可学习参数）

公式:
Attention(Q,K,V) = softmax( QKᵀ / sqrt(d_k) ) V
带 mask: 在 softmax 之前，把 mask 为 False 的位置的分数加上 −∞

步骤:
1. scores = einsum(Q, K, "... n d_k, ... m d_k -> ... n m") / sqrt(d_k)
2. if mask is not None: scores = scores.masked_fill(~mask, float("-inf"))
3. attn = 我自己的 softmax(scores, dim=-1)
4. return einsum(attn, V, "... n m, ... m d_v -> ... n d_v")

需要查的 API:
- einops.einsum（或 Q @ K.transpose(-2, -1)）
- Tensor.masked_fill(bool_mask, value) / torch.where
- math.sqrt；float("-inf")
- 广播规则（mask (n,m) 广播到 (..., n, m)）

边界情况:
- batch 维: "..." 可以是任意多维——3D 和 4D（多头）各有一个测试
- mask 语义反直觉: True = attend（信息流通），False = 屏蔽，别写反
- 屏蔽要在 softmax 之前加 -inf，不能在 softmax 之后乘 0（概率就不归一了）
- 缩放因子是 1/sqrt(d_k)，d_k 取 Q/K 最后一维
- False 位置的注意力权重应恰为 0；每行 True 位置权重和为 1

测试:
- 输出 shape 是否为 (..., n, d_v)？
- mask 全 True 时与不传 mask 一致？
- 手算 2x2: Q=K=[[1,0],[0,1]], V=[[1,2],[3,4]], 无mask ->
  scores/sqrt(2) -> softmax -> 加权和（自己在纸上乘一遍）
- uv run pytest -k test_scaled_dot_product_attention（3D）
- uv run pytest -k test_4d_scaled_dot_product_attention（4D）
```
