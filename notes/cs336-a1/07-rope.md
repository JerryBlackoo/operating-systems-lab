# 07 · 旋转位置编码（rope）

讲义 §3.4.3 ｜ 2 分 ｜ adapter: `adapters.run_rope` ｜ 测试: `uv run pytest -k test_rope`

```text
Problem:
RotaryPositionalEmbedding — 对 Q/K 施加旋转位置编码 (nn.Module，无可学习参数)

输入:
构造: theta: float（Θ，常取 10000）, d_k: int, max_seq_len: int, device=None
forward:
  x: (..., seq_len, d_k)
  token_positions: (..., seq_len)     x 中每个位置的绝对位置索引

输出:
(..., seq_len, d_k)   与输入同 shape

参数:
无可学习参数！cos/sin 用 register_buffer(persistent=False) 预计算缓存
buffer shape: (max_seq_len, d_k/2)

公式:
θ(i,k) = i / Θ^((2k-2)/d)      k = 1..d/2，i 为 token 位置
对每一对相邻元素 (x[2k-2], x[2k-1])（0 起下标）做 2D 旋转:
  x'_even = x_even·cos(θ) − x_odd·sin(θ)
  x'_odd  = x_even·sin(θ) + x_odd·cos(θ)

步骤:
1. init: inv_freq = Θ^(−arange(0, d_k, 2)/d_k)，shape (d_k/2,)
2. angles = outer(arange(max_seq_len), inv_freq) -> (max_seq_len, d_k/2)
   register_buffer("cos", angles.cos(), persistent=False)；sin 同理
3. forward: cos = self.cos[token_positions]，sin 同理  # 按位置索引切片
4. x_even = x[..., 0::2]，x_odd = x[..., 1::2]
5. 按旋转公式算出两半，再交错拼回原顺序
   （torch.stack((e', o'), dim=-1).flatten(-2) 或 rearrange）

需要查的 API:
- Module.register_buffer(name, tensor, persistent=False)
- torch.outer / torch.einsum
- 步长切片 x[..., 0::2] / x[..., 1::2]
- torch.stack + Tensor.flatten(-2)，或 einops.rearrange "... (p two) -> ..."
- 张量花式索引 cos[token_positions]（token_positions 可带 batch 维）

边界情况:
- 配对方式是"相邻两个元素"(2k-1, 2k)，不是前一半配后一半（HF 部分实现不同，别抄错）
- 只旋转 Q 和 K，不旋转 V
- head 维当作 batch 维：所有 head 用同一套 cos/sin
- 无可学习参数：buffer 不是 nn.Parameter，persistent=False 不进 state_dict
- 位置 0 时 cos=1、sin=0，变换应为恒等
- token_positions 可能带任意 batch 维，索引后 shape 要能广播到 x

测试:
- shape 是否不变？
- 位置 0 的输出是否等于输入（恒等）？
- 手算: d_k=2, Θ=10000, pos=1 -> 旋转角恰为 1 弧度，
  x=[1,0] -> [cos1, sin1]≈[0.5403, 0.8415]
- uv run pytest -k test_rope
```
