# 09 · 完全分片数据并行（fsdp）

讲义 §7 ｜ 15 分 ｜ adapter: `get_fsdp` ｜ 测试: `uv run pytest tests/test_fsdp.py`（建议跑 5 次）

```text
Problem:
FSDP 容器类 — 权重也分片：每 GPU 只存每个权重的 1/world_size 切片，
用前 all-gather 拼全，用完释放；梯度 reduce-scatter 回属主

输入:
__init__(module: torch.nn.Module, compute_dtype: torch.dtype | None = None)
forward(*inputs, **kwargs)
finish_gradient_synchronization()

输出:
包裹后的模块；参数/梯度/优化器状态显存都 ≈ 1/world_size
每个分片可直接配 A1 的标准 AdamW 使用

参数:
被分片的层: 所有 Linear 和 Embedding
不分片的层: norm 等小层（通信延迟不划算）

公式:
前向: W_full = all-gather({W^(i)}) -> 计算 -> 立即释放 W_full
反向: 再次 all-gather 权重 -> 算梯度 -> dW^(i) = reduce-scatter({dW})
预取: 第 k 层的 all-gather 在第 k-2 层前向完成后就发起
     （通信藏在计算后面，不阻塞关键路径）
混合精度: master 权重保持 FP32；提供 compute_dtype 时
     先 cast 再通信/计算（低精度通信省一半带宽）

步骤:
1. __init__: 遍历 module 找出 Linear/Embedding；把每个权重切成
   world_size 份，本 rank 只保留自己的分片（原参数替换/收纳）
2. 注册 forward pre-hook: 异步 all-gather 该层权重
   （调度上提前两层发起；hook 内 wait 保证可用）
3. 层前向完成后释放 gathered 全量权重（只留分片）
4. backward: 对应 pre-hook 再次 all-gather 权重供梯度计算
5. 梯度就绪后（post-accumulate-grad hook）异步 reduce-scatter，
   本 rank 只留自己分片的梯度；释放全量梯度
6. finish_gradient_synchronization(): 等全部通信 handle 完成
7. compute_dtype 给定时: cast 到低精度再通信/计算，
   master 权重与 optimizer 更新保持 FP32

需要查的 API:
- dist.all_gather_into_tensor / all_gather（拼权重）
- dist.reduce_scatter_tensor / reduce_scatter（分发梯度）
- module.register_forward_pre_hook / register_full_backward_pre_hook
- Tensor.register_post_accumulate_grad_hook
- async_op=True + handle.wait()
- Tensor.to(dtype)；torch.chunk / narrow（切分片）

边界情况:
- gathered 权重用完必须释放，否则显存等于没省
- 预取时机权衡: 太早占显存，太晚 GPU 等通信（讲义: 提前两层）
- norm 层不分片（太小，通信延迟 > 收益）
- reduce-scatter 语义: 输入是各 rank 的"完整"梯度部分和，
  输出是本 rank 分片上的"完整"和
- master FP32 不能丢（反复累加的值对精度敏感）
- 多次运行测试防 race；异步通信顺序要用 handle 管住

测试:
- adapter [adapters.get_fsdp]
- uv run pytest tests/test_fsdp.py（跑 ~5 次）
- 自检: 与 DDP 训练轨迹一致；nsys 里 all-gather 是否赶在前向前完成；
  峰值显存 vs DDP/优化器分片版的下降量符合预期
```
