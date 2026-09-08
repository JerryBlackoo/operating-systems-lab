# 11 · Transformer 块（transformer_block）

讲义 §3.5 ｜ 3 分 ｜ adapter: `adapters.run_transformer_block` ｜ 测试: `uv run pytest -k test_transformer_block`

```text
Problem:
TransformerBlock — pre-norm 结构: 注意力子层 + FFN 子层 (nn.Module)

输入:
构造: d_model: int, num_heads: int, d_ff: int（+ RoPE 的 max_seq_len, theta）
forward: x: (batch_size, seq_len, d_model)

输出:
(batch_size, seq_len, d_model)   同 shape

参数:
子模块（各自带自己的参数）:
  ln1: RMSNorm(d_model)      attn: MultiHeadSelfAttention(d_model, num_heads)
  ln2: RMSNorm(d_model)      ffn:  SwiGLU(d_model, d_ff)

公式:
pre-norm（先归一化再进子层，残差走"干净通道"）:
  z = x + MultiHeadSelfAttention(RMSNorm₁(x))
  y = z + FFN(RMSNorm₂(z))

步骤:
1. __init__ 里实例化 ln1 / attn / ln2 / ffn 四个子模块
2. z = x + self.attn(self.ln1(x))
3. y = z + self.ffn(self.ln2(z))
4. return y

需要查的 API:
- 无新 API —— 纯组装
- nn.Module 属性赋值即自动注册子模块与参数

边界情况:
- 是 pre-norm 不是 post-norm: norm 在子层"输入"上，残差相加后"不再"norm
- 两个 RMSNorm 参数独立，不共享
- 残差加的是"原始 x/z"，不是 norm 后的
- token_positions / RoPE 要一路传给 attn
- 别忘了 d_ff 传给 FFN（默认 8/3·d_model 取 64 倍数）

测试:
- shape 不变？dtype 不变？
- 载入测试提供的固定权重后输出与参考一致（load_state_dict 键名要对上）
- 把两个子层短路（输出置 0）时 block 应退化为恒等映射
- uv run pytest -k test_transformer_block
```
