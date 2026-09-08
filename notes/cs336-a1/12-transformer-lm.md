# 12 · 完整 Transformer 语言模型（transformer_lm）

讲义 §3.5 / 图 1 ｜ 3 分 ｜ adapter: `adapters.run_transformer_lm` ｜ 测试: `uv run pytest -k test_transformer_lm`

```text
Problem:
TransformerLM — Embedding -> N 个 Block -> RMSNorm -> LM head (nn.Module)

输入:
构造: vocab_size: int, context_length: int, num_layers: int,
      d_model, num_heads, d_ff, (rope_theta)
forward: token_ids: (batch_size, seq_len) 整数张量

输出:
logits: (batch_size, seq_len, vocab_size)   未归一化，"不要"做 softmax

参数:
token_embeddings: Embedding(vocab_size, d_model)
layers: num_layers × TransformerBlock
ln_final: RMSNorm(d_model)
lm_head: Linear(d_model, vocab_size)
（本作业不做 embedding 与 lm_head 权重共享）

公式:
h⁰ = Embedding(ids)
hˡ = TransformerBlockˡ(hˡ⁻¹)         l = 1..num_layers
logits = W_lm · RMSNorm(h^L)

步骤:
1. __init__: 建 embedding、nn.ModuleList([Block]*num_layers)、ln_final、lm_head
   context_length 决定 RoPE cos/sin buffer 的 max_seq_len
2. x = self.token_embeddings(token_ids)
3. for block in self.layers: x = block(x)
4. x = self.ln_final(x)
5. return self.lm_head(x)

需要查的 API:
- torch.nn.ModuleList（容器类是明确允许用的）
- 其余全部复用自己的模块

边界情况:
- 输出是 logits：softmax 留给 cross_entropy（训练）或 decoding（推理）
- pre-norm 架构要求最后有一个"额外的" RMSNorm，别漏
- context_length 只是 RoPE buffer 上限，forward 的 seq_len 可以更短
- 每个 Block 参数独立（不共享）
- 输入是整数 ID，不是 one-hot

测试:
- shape: (B, S) -> (B, S, vocab_size)？
- 载入参考权重后 logits 与期望一致？
- 参数量 sanity check: embedding V·d + 每层(4·d² attn + 3·d·d_ff) + d + d·V
- uv run pytest -k test_transformer_lm
```
