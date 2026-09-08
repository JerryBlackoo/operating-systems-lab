# 03 · 线性层（linear）

讲义 §3.3.2 ｜ 1 分 ｜ adapter: `adapters.run_linear` ｜ 测试: `uv run pytest -k test_linear`

```text
Problem:
Linear — 无 bias 的线性变换 (nn.Module)

输入:
x: (..., in_features)

输出:
(..., out_features)

参数:
W: (out_features, in_features)   注意存 W 本身，不是 W 的转置
初始化: N(0, σ²)，σ² = 2/(in_features+out_features)，截断到 [-3σ, 3σ]

公式:
数学(列向量): y = W x
代码(行主序): y = x @ W.T
或 einsum: einsum(x, W, "... d_in, d_out d_in -> ... d_out")

步骤:
1. 子类化 nn.Module，先调 super().__init__()
2. self.weight = nn.Parameter(torch.empty(out_features, in_features,
                                          device=device, dtype=dtype))
3. σ = sqrt(2/(in+out))；torch.nn.init.trunc_normal_(w, mean=0, std=σ, a=-3σ, b=3σ)
4. forward: 返回 einsum(x, W, "... d_in, d_out d_in -> ... d_out")

需要查的 API:
- torch.nn.Module / nn.Parameter
- torch.nn.init.trunc_normal_(tensor, mean, std, a, b)   std 是 σ 不是 σ²
- einops.einsum
- torch.empty(size, device=, dtype=)

边界情况:
- 没有 bias（本作业全程无 bias）
- 任意个前置 batch 维 (...) 都要支持
- device / dtype 参数要透传给参数张量
- 禁止 nn.Linear / nn.functional.linear
- trunc_normal_ 的截断参数 a、b 是绝对值边界（-3σ, 3σ），不是倍数

测试:
- shape: (..., in) -> (..., out)？
- dtype 与输入一致？
- 手算: W=[[1,2],[3,4]], x=[1,1] -> y=[3,7]
- uv run pytest -k test_linear（adapter 里用 load_state_dict 装载给定权重）
```
