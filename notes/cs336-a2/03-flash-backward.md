# 03 · FlashAttention-2 反向（flash_backward）

讲义 §4.2.2 ｜ 5 分 ｜ 测试: `uv run pytest -k test_flash_backward`

```text
Problem:
flash_backward — 用 L 重计算 P，避开 softmax 的注意力反向
（纯 PyTorch + torch.compile 即可，不必写 Triton）

输入:
从 ctx.saved_tensors 取: L, Q, K, V, O；autograd 传入 dO
（ctx.is_causal 决定是否重加因果 mask）

输出:
dQ: (batch, N_q, d)   dK: (batch, N_k, d)   dV: (batch, N_k, d)
（backward 返回值个数 = forward 输入个数: dQ, dK, dV, None←is_causal）

参数:
无

公式:
D = rowsum(O ∘ dO)  ∈ R^{N_q}        # 恒等式: rowsum(P ∘ dP) = rowsum(O ∘ dO)
S = Q K^T / sqrt(d)                   # 重计算（causal 时重加 mask）
P = exp(S − L)                        # L 按行广播，天然完成 softmax 归一化
dV = P^T dO
dP = dO V^T
dS = P ∘ (dP − D)                     # D 按行广播
dQ = dS K / sqrt(d)
dK = dS^T Q / sqrt(d)

步骤:
1. 写一个普通 PyTorch 函数 flash_bwd(Q, K, V, O, dO, L, is_causal)
2. 先算 D = (O * dO).sum(-1)
3. 依公式顺序算 S -> P -> dV -> dP -> dS -> dQ -> dK（einsum 表达最清晰）
4. causal 时: S 加与前向一致的 -1e6 mask 再 exp
5. 用 torch.compile 包一层（融合 kernel，减少中间张量 IO）
6. 在 autograd.Function.backward(ctx, dO) 里调它并按位置返回梯度

需要查的 API:
- torch.compile
- ctx.saved_tensors / ctx.is_causal
- einops.einsum（"... q d, ... k d -> ... q k" 等）
- Tensor.sum(dim=-1, keepdim=True)（D、广播）

边界情况:
- 别忘了 D 向量！它替代了 softmax 反向中的 (diag(P) − P P^T) 项
- causal: backward 重算 S 时必须加与 forward 相同的 mask，
  否则被屏蔽位置 P = exp(S − L) 不为 0，梯度就错了
- L 是 logsumexp（含 m + log l），P = exp(S − L) 已经归一化，无需再除
- backward 返回元组要与 forward 输入一一对应（is_causal 位置返回 None）
- 中间量用 fp32 精度更稳

测试:
- uv run pytest -k test_flash_backward
- 自检: 与朴素 attention 的 torch.autograd.grad 结果逐元素接近
- 手算: 1x1 tile 情形退化为标准 softmax 反向公式
```
