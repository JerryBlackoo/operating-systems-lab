# 16 · 梯度裁剪（gradient_clipping）

讲义 §4.5 ｜ 1 分 ｜ adapter: `adapters.run_gradient_clipping` ｜ 测试: `uv run pytest -k test_gradient_clipping`

```text
Problem:
gradient_clipping — 把"全体参数梯度"的总 L2 范数限制在 M 以内（纯函数）

输入:
parameters: Iterable[nn.Parameter]   模型全部参数
max_l2_norm: M                       范数上限

输出:
无 —— 原地修改每个 p.grad

参数:
ε = 1e-6（PyTorch 默认，加在分母防除零）

公式:
‖g‖₂ = sqrt( Σ_p ‖p.grad‖₂² )        # 所有参数梯度拼成一个大向量的总范数
若 ‖g‖₂ < M: 不动
否则:        g ← g · M / (‖g‖₂ + ε)   # 裁剪后范数略小于 M

步骤:
1. grads = [p.grad for p in parameters if p.grad is not None]
2. total_norm = sqrt( sum( (g**2).sum() for g in grads ) )
3. if total_norm >= M:
       scale = M / (total_norm + 1e-6)
       for g in grads: g.mul_(scale)

需要查的 API:
- Tensor.pow(2).sum() / torch.linalg.vector_norm
- Tensor.mul_（原地乘）
- p.grad（可能为 None）
- （对拍用）torch.nn.utils.clip_grad_norm_

边界情况:
- 是"所有参数合并"的总范数，不是每个参数各自 clip！
- 原地修改 p.grad，不返回新张量
- ε 加在分母 -> 裁剪后范数是 M·‖g‖/(‖g‖+ε)，略小于 M
- norm < M 时什么都不做（不缩放）
- p.grad is None 的参数跳过
- 在 backward 之后、optimizer.step 之前调用

测试:
- 裁剪后总范数 ≈ M（略小）？
- 小梯度（norm < M）是否被完全保留？
- 与 torch.nn.utils.clip_grad_norm_ 行为一致？
- 手算: 两个参数梯度 [3] 和 [4]，总范数 5，M=1 -> 缩放 1/5 -> [0.6], [0.8]
- uv run pytest -k test_gradient_clipping
```
