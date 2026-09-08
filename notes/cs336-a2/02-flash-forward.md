# 02 · FlashAttention-2 前向（flash_forward）

讲义 §4.2.2 / 算法 1 ｜ 15 分 ｜ adapters: `get_flashattention_autograd_function_pytorch` / `get_flash_autograd_function_triton` ｜ 测试: `uv run pytest -k test_flash_forward_pass_pytorch` 和 `-k test_flash_forward_pass_triton`

```text
Problem:
flash_forward — 分块 + 在线 softmax 的注意力前向
(a) 纯 PyTorch 分块版（调试用） (b) Triton 融合 kernel (c) causal 开关

输入:
Q: (batch, N_q, d)   K, V: (batch, N_k, d)   is_causal: bool = False
tile 大小 B_q, B_k（自选，至少 16x16；测试维度均为 2 的幂且 ≥16）

输出:
O: (batch, N_q, d)
L: (batch, N_q)   逐行 logsumexp（存给 backward 重计算用）
forward 需 ctx.save_for_backward(L, Q, K, V, O) 并返回 O

参数:
无可学习参数（这是一个 torch.autograd.Function，不是 Module）

公式:
(在线 softmax，对每个 query tile i 扫过 key tiles j = 1..T_k)
S_i^(j) = Q_i K^(j)T / sqrt(d)                       (B_q, B_k)
m_i^(j) = max(m_i^(j-1), rowmax(S_i^(j)))             运行最大值
P~ = exp(S_i^(j) − m_i^(j))                           未归一化概率
l_i^(j) = exp(m_i^(j-1) − m_i^(j)) · l_i^(j-1) + rowsum(P~)   运行分母
O_i^(j) = diag(exp(m_i^(j-1) − m_i^(j))) O_i^(j-1) + P~ V^(j)  运行输出
收尾: O_i = diag(l_i)^-1 · O_i^(T_k)；L_i = m_i + log(l_i)
初值: m = -inf, l = 0, O = 0

步骤:
(a) PyTorch 版: 写 forward(ctx, Q, K, V, is_causal=False)，
    用 python 双层循环按上面公式分块计算；backward 先 raise NotImplementedError；
    与朴素 attention（含 L=logsumexp(S)）对拍
(b) Triton 版:
    1. launch grid = (T_q, batch_size)：每个程序实例负责一个 batch 的一个 query tile
    2. kernel 参数: Q/K/V/O/L 指针 + 各自 strides + N_QUERIES/N_KEYS +
       scale(=1/sqrt(d)) + D/Q_TILE_SIZE/K_TILE_SIZE (tl.constexpr)
    3. 指针加 batch 偏移后 tl.make_block_ptr 建 Q/K/V/O/L 块指针
    4. 单循环 j=1..T_k: tl.load K/V tile -> tl.dot 算 S -> 更新 m/P~/l/O
       -> 循环末尾 advance K/V 块指针
    5. 循环后归一化 O、算 L，tl.store 写回
(c) causal: kernel 加 is_causal: tl.constexpr 参数；
    用 query/key 全局索引向量比较构造 (B_q, B_k) mask，
    被屏蔽处 S += -1e6；autograd.Function 里 ctx.is_causal = is_causal

需要查的 API:
- torch.autograd.Function: forward(ctx, ...) / ctx.save_for_backward / .apply
- triton.jit；tl.program_id(axis)；triton.cdiv
- tl.make_block_ptr(base, shape, strides, offsets, block_shape, order)
- tl.load / tl.store(boundary_check=, padding_option="zero")
- block_ptr.advance((dr, dc))
- tl.dot(a, b, acc=acc)；tl.zeros / tl.full；tl.maximum / tl.max(axis)
- tl.exp / tl.log；tl.arange（构造索引做 causal mask）
- tensor.to(dtype)；block_ptr.type.element_ty（取目标 dtype）

边界情况:
- 片上缓冲 (O, l, m) 一律 tl.float32；累加用 tl.dot 的 acc 参数
- P~ 与 V 相乘前 cast 成 V 的 dtype；O 写回前 cast 成输出指针的 dtype
- causal 屏蔽加 -1e6（不是 -inf，避免 NaN）
- m 初值 -inf：第一轮 exp(m_old − m_new) = exp(-inf) = 0，正好清零旧值
- 每个实例只读写自己 batch/自己 query tile 的 Q、O、L
- L 必须写回全局内存（backward 依赖它，不再存 P）

测试:
- (a) uv run pytest -k test_flash_forward_pass_pytorch
- (b) uv run pytest -k test_flash_forward_pass_triton
- causal 默认 False，旧测试仍须通过
- 自检: 与朴素 softmax(QK^T/sqrt(d))V 输出逐元素接近；
  L 与 torch.logsumexp(S, dim=-1) 一致
```
