# 04 · 词嵌入层（embedding）

讲义 §3.3.3 ｜ 1 分 ｜ adapter: `adapters.run_embedding` ｜ 测试: `uv run pytest -k test_embedding`

```text
Problem:
Embedding — token ID 查表得到向量 (nn.Module)

输入:
token_ids: (batch_size, sequence_length)   整数张量 (torch.LongTensor)

输出:
(batch_size, sequence_length, d_model)     浮点张量

参数:
weight: (num_embeddings, embedding_dim) = (vocab_size, d_model)
        d_model 必须放在最后一维
初始化: N(0, 1)，截断到 [-3, 3]

公式:
output[b, s, :] = weight[token_ids[b, s], :]
（就是按行取矩阵的行，无矩阵乘法）

步骤:
1. 子类化 nn.Module，super().__init__()
2. self.weight = nn.Parameter(torch.empty(num_embeddings, embedding_dim,
                                          device=device, dtype=dtype))
3. trunc_normal_(weight, mean=0, std=1, a=-3, b=3)
4. forward: return self.weight[token_ids]   # 花式索引天然支持批量维

需要查的 API:
- 张量整数(花式)索引: weight[ids] —— ids 任意 shape，输出 shape = ids.shape + (d_model,)
- nn.Parameter / torch.nn.init.trunc_normal_
- torch.empty(device=, dtype=)

边界情况:
- 禁止 nn.Embedding / nn.functional.embedding
- 输入是整数 dtype（long），输出是参数的浮点 dtype
- 存储顺序是 (vocab_size, d_model)，别转置
- token_ids 任意 batch 维都可以（索引自动处理）

测试:
- shape: (B, S) -> (B, S, d_model)？
- dtype: 输出为浮点（与 weight 一致）？
- 手算: token_ids=[[0]] -> 输出等于 weight 第 0 行？
- uv run pytest -k test_embedding
```
