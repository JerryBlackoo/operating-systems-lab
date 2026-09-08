# 04 · 单节点分布式通信基准（distributed_communication_single_node）

讲义 §5.1 ｜ 5 分 ｜ 无 pytest（交付物是脚本 + 图表，分布式编程的 hello world）

```text
Problem:
distributed_communication_single_node — 多进程 all-reduce 延迟基准

输入:
- backend: "gloo"(CPU 调试) / "nccl"(GPU 必用)
- 数据大小: float32 张量 1MB / 10MB / 100MB / 1GB
- world_size: 2 / 4 / 6 个进程(GPU)

输出:
各(backend × size × world_size)组合的 all-reduce 耗时表/图

参数:
无

公式:
环形 all-reduce 理论时间 = 2·(N−1)/N · S/W
（S=数据字节数, W=每设备出口带宽, N=设备数——后面书面题会用到）

步骤:
1. setup(rank, world_size): 设 os.environ["MASTER_ADDR"]="localhost"、
   ["MASTER_PORT"]="29500"，dist.init_process_group(backend, rank=, world_size=)
2. main: mp.spawn(fn=worker, args=(world_size,), nprocs=world_size, join=True)
   （worker 的第一个位置参数自动是 rank）
3. worker 内: 建张量；GPU 时 torch.cuda.set_device(rank) 或 to(f"cuda:{rank}")
4. warmup ≥5 次 all-reduce；然后计时多次:
   start -> dist.all_reduce(data, async_op=False) -> torch.cuda.synchronize() -> stop
5. 用 dist.all_gather_object 收集所有 rank 的计时，汇总均值

需要查的 API:
- torch.distributed: init_process_group / all_reduce / all_gather_object / barrier
- torch.multiprocessing.spawn
- torch.cuda.set_device / synchronize
- os.environ（MASTER_ADDR / MASTER_PORT）

边界情况:
- NCCL 只支持 GPU 张量；Gloo 用于本地 CPU 调试——切换只改 backend + 设备
- async_op=False 返回 ≠ 通信完成（只是入队），GPU 上仍必须 synchronize
- 每个 rank 必须用不同 GPU，否则互相争抢
- 各 rank 计时有抖动，聚合所有 rank 的测量再统计
- 输出顺序不确定是正常的；唯一保证是 all-reduce 后各 rank 数据 bitwise 相同

测试:
- 无 pytest；自检:
- all-reduce 后各 rank 张量是否完全一致？
- 耗时是否随数据量近似线性增长？NCCL 是否显著快于 Gloo？
```
