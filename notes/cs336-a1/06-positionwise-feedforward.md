# 06 · SwiGLU 前馈网络（positionwise_feedforward）

讲义 §3.4.2 ｜ 2 分 ｜ adapter: `adapters.run_swiglu` ｜ 测试: `uv run pytest -k test_swiglu`

```text
Problem:
SwiGLU FFN — SiLU 激活 + 门控线性单元 (nn.Module)

输入:
x: (..., d_model)

输出:
(..., d_model)

参数:
W1: (d_ff, d_model)     门控分支（过 SiLU）
W3: (d_ff, d_model)     线性分支
W2: (d_model, d_ff)     输出投影
d_ff ≈ (8/3)·d_model，且取 64 的倍数（如 d_model=512 -> d_ff=1344）

公式:
SiLU(x) = x · σ(x) = x / (1 + e^(-x))
FFN(x) = SwiGLU(x) = W2 ( SiLU(W1 x) ⊙ W3 x )      ⊙ = 逐元素乘

步骤:
1. 用自己实现的 Linear 建 w1(d_model->d_ff), w3(d_model->d_ff), w2(d_ff->d_model)
2. gate = w1(x)；gate = gate * torch.sigmoid(gate)   # SiLU，允许用 torch.sigmoid
3. h = gate * w3(x)                                  # 逐元素门控
4. return w2(h)

需要查的 API:
- torch.sigmoid（本题明确允许，为数值稳定）
- 逐元素乘 * （广播规则）
- 复用自己的 Linear 模块

边界情况:
- 三个矩阵方向别搞混: W1/W3 是升维 (d_ff, d_model)，W2 是降维 (d_model, d_ff)
- d_ff 不是 4·d_model 而是 8/3·d_model（因为有 3 个矩阵，参数量对齐），
  且向 64 的倍数取整（硬件效率）
- 全程无 bias
- SiLU ≠ ReLU：零点附近平滑、允许小负值
- 任意 batch 维 (...) 支持（由 Linear 保证）

测试:
- shape 进出一致？dtype 一致？
- 手算: 标量验证 SiLU(1)=1·σ(1)≈0.731
- 门控为 0 时输出应为 0（gate=0 -> h=0）
- uv run pytest -k test_swiglu
```
