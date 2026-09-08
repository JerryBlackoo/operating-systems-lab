# 08 · Softmax（softmax）

讲义 §3.4.4 ｜ 1 分 ｜ adapter: `adapters.run_softmax` ｜ 测试: `uv run pytest -k test_softmax_matches_pytorch`

```text
Problem:
softmax — 沿指定维度把分数变成概率分布（纯函数）

输入:
x: 任意 shape 的张量
dim: int   要归一化的维度

输出:
与 x 同 shape；沿 dim 维每组元素非负、和为 1

参数:
无

公式:
softmax(v)ᵢ = exp(vᵢ) / Σⱼ exp(vⱼ)
数值稳定版（softmax 对整体加常数不变）:
softmax(v)ᵢ = exp(vᵢ − max(v)) / Σⱼ exp(vⱼ − max(v))

步骤:
1. m = x.max(dim=dim, keepdim=True).values     # 注意 .values！
2. e = torch.exp(x − m)
3. return e / e.sum(dim=dim, keepdim=True)

需要查的 API:
- Tensor.max(dim, keepdim) —— 返回 (values, indices) 命名元组，要取 .values
- Tensor.sum(dim, keepdim)
- torch.exp

边界情况:
- 不减 max 时 exp(1000)=inf -> inf/inf=NaN，必须减
- keepdim=True 保证减法/除法能广播
- dim 可以是任意维（不只 -1），也可以是负数索引
- 减 max 后最大项为 exp(0)=1，分母 ≥ 1，不会除零
- 一行全为 -inf 会得 NaN（causal mask 下对角线恒为 True，不会发生）

测试:
- shape 是否不变？沿 dim 求和是否为 1？
- 与 torch.softmax(x, dim) 是否一致？
- 大数输入 x=[1000, 1000] 是否输出 [0.5, 0.5] 而非 NaN？
- uv run pytest -k test_softmax_matches_pytorch
```
