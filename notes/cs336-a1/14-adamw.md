# 14 · AdamW 优化器（adamw）

讲义 §4.3 / 算法 1 ｜ 2 分 ｜ adapter: `adapters.get_adamw_cls` ｜ 测试: `uv run pytest -k test_adamw`

```text
Problem:
AdamW — 解耦权重衰减的 Adam（torch.optim.Optimizer 子类）

输入:
__init__(params, lr=α, betas=(β1, β2), eps=ε, weight_decay=λ)
典型值: β=(0.9, 0.999) 或 LLM 常用 (0.9, 0.95)，ε=1e-8
step() 在 backward 之后调用，读 p.grad 原地改 p.data

输出:
无（原地更新参数）；step 返回 closure 的 loss（可为 None）

参数:
每个参数 p 的状态 state[p]: t（步数）, m（一阶矩）, v（二阶矩）
m、v 与 p 同 shape，初始为 0

公式:
（每步，t 从 1 开始）
g   = p.grad
θ  ←  θ − α·λ·θ                        # 权重衰减：用原始 α，与梯度解耦
m  ←  β1·m + (1−β1)·g
v  ←  β2·v + (1−β2)·g²                  # 逐元素平方
α_t = α · sqrt(1 − β2^t) / (1 − β1^t)   # 偏差修正
θ  ←  θ − α_t · m / (sqrt(v) + ε)

步骤:
1. __init__: 校验 lr ≥ 0 等；defaults = {"lr":…, "betas":…, "eps":…,
   "weight_decay":…}；super().__init__(params, defaults)
2. step: for group in self.param_groups -> for p in group["params"]
3. p.grad 为 None 则 continue；state = self.state[p]，
   首次访问时初始化 t=0, m=zeros_like(p), v=zeros_like(p)
4. t = state["t"] + 1，按公式更新（可用 mul_/add_/addcmul_ 原地算）
5. state 写回 t/m/v

需要查的 API:
- torch.optim.Optimizer 基类: super().__init__(params, defaults)、
  self.param_groups、self.state（dict[Parameter, dict]）
- torch.zeros_like
- 原地操作: mul_ / add_ / addcmul_ / sqrt
- math.sqrt（标量偏差修正项）

边界情况:
- t 从 1 开始计（β^t 的偏差修正；t=0 会除零）
- 权重衰减是"解耦"的: 直接 θ−=α·λ·θ，不能加进梯度里；且用 α 不是 α_t
- m、v 是逐参数张量状态，存在 self.state[p] 里（checkpoint 时会被存下）
- p.grad is None 的参数要跳过
- 分母是 sqrt(v)+ε，ε 在 sqrt 外面

测试:
- 与测试提供的参考权重轨迹一致（跑几步比对）？
- state 是否被 state_dict() 正确导出（配合 checkpointing）？
- 手算 1 步: p=1, g=1, α=1, β=(0.9,0.999), λ=0 ->
  m=0.1, v=0.001, α_1=1·sqrt(0.001)/0.1 -> Δθ≈1
- uv run pytest -k test_adamw
```
