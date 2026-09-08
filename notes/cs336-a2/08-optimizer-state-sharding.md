# 08 · 优化器状态分片（optimizer_state_sharding）

讲义 §6 ｜ 15 分 ｜ adapter: `get_sharded_optimizer` ｜ 测试: `uv run pytest tests/test_sharded_optimizer.py`（建议跑 5 次）

```text
Problem:
sharded optimizer — 每个 rank 只维护 ~1/world_size 参数的优化器状态
（简化版 ZeRO stage 1）

输入:
__init__(params, optimizer_cls: Type[Optimizer], **kwargs)
    params: 参数或参数组集合（会被分片到各 rank）
    optimizer_cls: 被包裹的优化器类型（如 AdamW）
    kwargs: 透传给 optimizer_cls 的超参
step(closure, **kwargs)
add_param_group(param_group: dict)

输出:
一个 torch.optim.Optimizer 子类实例；显存中优化器状态降为 ~1/world_size

参数:
内部持有一个只管本 rank 参数子集的真实 optimizer 实例

公式:
AdamW 状态 = 2 倍参数量(m+v) => 分片后每 rank 状态 ≈ 2·P/world_size
step 后需同步: 每个参数由其"属主 rank"broadcast 更新后的值给所有 rank

步骤:
1. __init__: 先记下 rank/world_size/optimizer_cls/kwargs、
   初始化空的参数->属主映射，再调 super().__init__(params, defaults)
   （注意: 基类构造器内部会回调 add_param_group！属性必须先赋好）
2. add_param_group(group): 遍历 group["params"]，
   按轮询 idx % world_size（或按元素数均衡）指定属主 rank；
   属于本 rank 的参数放进内层 optimizer（建实例或 add_param_group）；
   同时调 super().add_param_group(group) 记录全量参数
3. step: self.inner.step(closure, **kwargs)      # 只更新本 rank 分片
4. step 后同步: for p, owner in 参数属主映射:
       dist.broadcast(p.data, src=owner)

需要查的 API:
- torch.optim.Optimizer 基类: __init__(params, defaults) / add_param_group /
  self.param_groups
- dist.broadcast(tensor, src=owner_rank)
- typing.Type[Optimizer]（类作为参数传入）
- dist.get_rank() / dist.get_world_size()

边界情况:
- 初始化顺序陷阱: super().__init__ 会立刻调 add_param_group，
  此时你的自定义属性必须已存在（否则 AttributeError）
- 分片要均衡（按参数张量个数轮询即可，更好是按 numel）
- step 后必须同步"所有"参数（每个参数从它的属主广播）
- add_param_group 训练中途也可能被调（逐层解冻场景）
- 与 ZeRO-1 的区别: ZeRO-1 用 reduce-scatter 梯度 + all-gather 参数，
  通信量更优；我们的简化版 all-reduce 全梯度 + broadcast 参数

测试:
- adapter [adapters.get_sharded_optimizer]
- uv run pytest tests/test_sharded_optimizer.py（跑 ~5 次）
- 自检: 训练轨迹与不分片的 AdamW 逐步一致；
  峰值显存下降 ≈ 优化器状态 × (1 − 1/world_size)
```
