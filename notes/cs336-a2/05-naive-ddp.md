# 05 · 朴素数据并行（naive_ddp）

讲义 §5.2 ｜ 5 分 ｜ adapter: `get_ddp`（可选 `ddp_on_after_backward`）｜ 测试: `uv run pytest tests/test_ddp.py`

```text
Problem:
naive_ddp — backward 结束后逐参数 all-reduce 梯度的最小 DDP

输入:
任意 nn.Module + 已初始化的进程组（world_size 个 rank）
每 rank 分到 batch 的 1/world_size（n/d 条样本，d 整除 n）

输出:
各 rank 参数每步保持 bitwise 同步的训练流程

参数:
无新增参数（复制的是同一个模型）

公式:
不变式: 相同初始参数 + 相同平均梯度 + 相同优化器
        => 各 rank 参数/优化器状态永远同步
梯度: g = (1/world_size) · Σ_rank g_rank      # all-reduce SUM 后除以 d

步骤:
1. 训练前: for p in model.parameters(): dist.broadcast(p.data, src=0)
   （保证所有 rank 从 rank0 的随机初始化出发）
2. 每步: 各 rank 用自己的 batch 分片 forward + backward
3. backward 后: for p in model.parameters():
       if p.grad is not None:
           dist.all_reduce(p.grad, op=dist.ReduceOp.SUM)
           p.grad /= world_size
4. 各 rank 各自 optimizer.step()（状态自然保持一致）

需要查的 API:
- dist.broadcast(tensor, src=0)
- dist.all_reduce(tensor, op=dist.ReduceOp.SUM)   # 或 ReduceOp.AVG (NCCL)
- mp.spawn / init_process_group（复用上一张卡）
- model.parameters()；p.grad

边界情况:
- broadcast 必须在训练开始前做一次，否则各 rank 初始权重不同、永不同步
- 是"平均"不是"求和"——不除 world_size 等于 lr 放大 d 倍
- batch 必须整除 world_size，否则最慢的 rank 拖住所有人
- 优化器不需要任何通信（同输入 -> 同状态）
- 逐参数 all-reduce = 每个张量一次通信调用，开销大（后续卡片优化）

测试:
- adapter [adapters.get_ddp]（可选 [adapters.ddp_on_after_backward]）
- uv run pytest tests/test_ddp.py
- 自检: 单进程全 batch 训练 vs 2 进程 DDP，若干步后权重应逐元素一致
```
