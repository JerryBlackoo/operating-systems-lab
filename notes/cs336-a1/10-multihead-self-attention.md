# 10 · 因果多头自注意力（multihead_self_attention）

讲义 §3.4.5 ｜ 5 分 ｜ adapter: `adapters.run_multihead_self_attention` ｜ 测试: `uv run pytest -k test_multihead_self_attention`

```text
Problem:
MultiHeadSelfAttention — 带因果 mask 与 RoPE 的多头自注意力 (nn.Module)

输入:
构造: d_model: int, num_heads: int（RoPE 版还需 max_seq_len, theta）
forward: x: (..., seq_len, d_model)，可选 token_positions

输出:
(..., seq_len, d_model)

参数:
W_Q: (h·d_k, d_model)    W_K: (h·d_k, d_model)
W_V: (h·d_v, d_model)    W_O: (d_model, h·d_v)
其中 d_k = d_v = d_model / num_heads（4 个自己的 Linear）

公式:
MultiHead(x) = W_O · Concat(head_1, ..., head_h)
head_i = Attention(Q_i, K_i, V_i)     每头独立做缩放点积注意力
因果 mask: 位置 i 只能注意 j ≤ i（下三角为 True，含对角线）

步骤:
1. Q = W_Q(x), K = W_K(x), V = W_V(x)   # 只有 3 次矩阵乘，一次算出所有头
2. rearrange(Q, "... s (h d) -> ... h s d", h=num_heads)   # K、V 同理
3. 对 Q、K 施加 RoPE（V 不加！head 维当 batch 维，各头旋转相同）
   token_positions 缺省用 arange(seq_len)
4. mask = 下三角 True 的 (s, s) bool 矩阵
   （torch.arange(s)[:, None] >= torch.arange(s)[None, :] 或 torch.tril/triu）
5. out = scaled_dot_product_attention(Q, K, V, mask)
6. rearrange(out, "... h s d -> ... s (h d)") 后过 W_O 返回

需要查的 API:
- einops.rearrange（拆头/合头）
- torch.tril / torch.triu / arange 广播比较（构造因果 mask）
- torch.ones(s, s, dtype=torch.bool)
- 复用: 自己的 Linear、RoPE、scaled_dot_product_attention、softmax

边界情况:
- d_k = d_v = d_model // num_heads，要求整除
- mask 方向: True 在下三角（j ≤ i），对角线为 True（token 能注意自己）
- RoPE 只旋 Q/K；且 head 维是 batch 维（同一位置所有头旋转角一致）
- 拆头时 (h d) 的顺序要与权重排布一致（W_Q 输出按头分块）
- 任意前置 batch 维 (...)

测试:
- 输出 shape 与输入 x 相同？
- 因果性: 改动位置 j>i 的输入，位置 i 的输出不应变化
- num_heads=1 时应退化为单头注意力
- uv run pytest -k test_multihead_self_attention
```
