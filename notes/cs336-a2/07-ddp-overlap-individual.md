# 07 · 通信计算重叠 DDP 容器（ddp_overlap_individual_parameters）

讲义 §5.3.2 ｜ 5 分 ｜ adapters: `get_ddp` / `ddp_on_after_backward`（可选）｜ 测试: `uv run pytest tests/test_ddp.py`（建议跑 5 次）

```text
Problem:
DDP 容器类 — 梯度一就绪就异步 all-reduce，与 backward 计算重叠

输入:
__init__(module: torch.nn.Module)
forward(*inputs, **kwargs)                      透传给被包裹模块
finish_gradient_synchronization()               step 前调用，等待通信完成

输出:
用法:
  ddp_model = DDP(model)
  loss = loss_fn(ddp_model(x), y)
  loss.backward()
  ddp_model.finish_gradient_synchronization()
  optimizer.step()

参数:
无新增可学习参数（包裹原模块）

公式:
关键观察: backward 从 loss 向输入逐层算梯度，
后面层的梯度先就绪 -> 可立即开始通信，与前面层的计算并行
=> 通信时间被藏进计算时间里

步骤:
1. __init__: 保存 module；for p in module.parameters():
   dist.broadcast(p.data, src=0)                 # 初始参数同步
2. 给每个需要梯度的参数注册钩子:
   p.register_post_accumulate_grad_hook(hook)
3. hook(p): p.grad /= world_size；
   handle = dist.all_reduce(p.grad, async_op=True)；
   self.handles.append(handle)
4. forward: return self.module(*inputs, **kwargs)
5. finish_gradient_synchronization():
   for h in self.handles: h.wait()
   self.handles.clear()

需要查的 API:
- Tensor.register_post_accumulate_grad_hook（梯度累积完成后触发）
- dist.all_reduce(..., async_op=True) -> handle；handle.wait()
- dist.broadcast
- nn.Module.parameters() / named_parameters()

边界情况:
- hook 触发时机 = 该参数梯度"累积完毕"后（安全通信点）
- 先除 world_size 再通信（或统一 reduce 后除，但要一致）
- handle.wait() 只保证操作已入队 GPU，对后续依赖它的 op 已足够
- 每次迭代后必须清空 handles 列表，否则越积越多
- 忘调 finish_gradient_synchronization 就 step -> 用了半新不旧的梯度（race）
- 共享参数（如果有 weight tying）只注册一次 hook

测试:
- adapters [get_ddp] / [ddp_on_after_backward]（后者视实现可选）
- uv run pytest tests/test_ddp.py，跑 ~5 次确认无 race
- 自检: 训练结果与 naive DDP 一致；nsys 时间线上通信与 backward 重叠
```
