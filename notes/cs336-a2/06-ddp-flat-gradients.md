# 06 · 扁平化梯度 DDP（minimal_ddp_flat_benchmarking）

讲义 §5.3.1 ｜ 2 分 ｜ 无 pytest（改造 naive_ddp + 基准对比）

```text
Problem:
minimal_ddp_flat — 把所有参数梯度拼成一个大张量，只发一次 all-reduce

输入:
naive_ddp 的训练流程（1 节点 × 2 GPU，xl 模型）

输出:
每步训练时间 + 通信时间，与逐参数版对比

参数:
无

公式:
通信调用次数: N 个参数 -> 1 次
每次通信调用有固定开销(launch/握手)，小张量多次调用时开销占比高
代价: flatten/unflatten 需要一次额外拷贝（瞬时显存 + 一份梯度大小）

步骤:
1. backward 结束后收集 grads = [p.grad for p in params if p.grad is not None]
2. flat = torch._utils._flatten_dense_tensors(grads)     # 拼成 1D 大张量
3. dist.all_reduce(flat, op=SUM)；flat /= world_size
4. unflat = torch._utils._unflatten_dense_tensors(flat, grads)
5. for g, u in zip(grads, unflat): g.copy_(u)             # 写回各参数
6. optimizer.step()
7. 分别计时"整步"和"通信段"（通信段前后 synchronize）

需要查的 API:
- torch._utils._flatten_dense_tensors / _unflatten_dense_tensors
- Tensor.copy_（原地写回）
- torch.cuda.synchronize（分段计时）

边界情况:
- unflatten 返回的是新张量，必须 copy_ 回 p.grad（不能只换引用）
- flatten 要求同 dtype；混合 dtype 参数要分组处理
- 峰值显存多一份梯度大小的拷贝
- 对比实验必须同条件: 1 节点 2 GPU、xl 模型、相同 batch

测试:
- 无 pytest；自检:
- 训练结果与逐参数 all-reduce 完全一致（数值上等价）？
- 通信时间: 1 次大调用 vs N 次小调用哪个快？差多少？
```
