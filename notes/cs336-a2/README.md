# CS336 Assignment 2 (systems) — 编程题速学卡片

来源: CS336 Spring 2026, Assignment 2: Systems and Parallelism (v26.1.3)。
每道**编程题**一张“手写纸”。配套仓库: [stanford-cs336/assignment2-systems](https://github.com/stanford-cs336/assignment2-systems)。

## 全局背景

- 本作业基于 A1 的模型（`cs336-basics/` 提供 staff 参考实现，可换成自己的）。
- 模型尺寸表（Table 1，vocab 10000、batch 4、ctx 默认 512）：small(768) / medium(1024) / large(1280) / xl(2560) / 10B(4608)。
- 两大主题：单卡性能（profiling、Triton kernel、混合精度、激活检查点）+ 多卡并行（DDP、优化器分片、FSDP）。
- 分布式调试口诀：本地用 Gloo + CPU 调通，再上 NCCL + GPU 测性能；异步测速必 `torch.cuda.synchronize()`。

## 编程题卡片索引（9 张）

| # | 卡片 | Problem | 分值 | 讲义 | 测试 |
|---|------|---------|------|------|------|
| 01 | [benchmarking_script](01-benchmarking-script.md) | 端到端基准测试脚本 | 4 | §2.1.3 | 无 pytest（基础设施） |
| 02 | [flash_forward](02-flash-forward.md) | FlashAttention-2 前向（PyTorch+Triton） | 15 | §4.2.2 | `-k test_flash_forward_pass_pytorch` / `-k test_flash_forward_pass_triton` |
| 03 | [flash_backward](03-flash-backward.md) | FlashAttention-2 反向（重计算） | 5 | §4.2.2 | `-k test_flash_backward` |
| 04 | [distributed_communication_single_node](04-distributed-communication.md) | all-reduce 通信基准 | 5 | §5.1 | 无 pytest |
| 05 | [naive_ddp](05-naive-ddp.md) | 朴素 DDP | 5 | §5.2 | `uv run pytest tests/test_ddp.py` |
| 06 | [minimal_ddp_flat_benchmarking](06-ddp-flat-gradients.md) | 扁平化梯度 DDP | 2 | §5.3.1 | 无 pytest（基准对比） |
| 07 | [ddp_overlap_individual_parameters](07-ddp-overlap-individual.md) | 通信计算重叠 DDP 容器 | 5 | §5.3.2 | `uv run pytest tests/test_ddp.py` ×5 |
| 08 | [optimizer_state_sharding](08-optimizer-state-sharding.md) | 优化器状态分片（ZeRO-1 简化版） | 15 | §6 | `uv run pytest tests/test_sharded_optimizer.py` ×5 |
| 09 | [fsdp](09-fsdp.md) | 完全分片数据并行 | 15 | §7 | `uv run pytest tests/test_fsdp.py` ×5 |

依赖链：01 是所有 profiling 实验的基础设施；02→03 构成完整 FA2；04→05→06→07 逐步优化 DDP；07→08→09 逐级分片（梯度同步 → 优化器状态 → 权重）。

## 非编程题清单（profiling 实验 / 书面计算）

| Problem | 类型 | 分值 | 内容 |
|---------|------|------|------|
| nsys_profile | 实验 | 5 | Nsight Systems 分析 kernel 耗时（NVTX 标注、过滤 warmup） |
| mixed_precision_accumulation | 小实验 | 1 | fp16/fp32 累加精度对比（为何累加要高精度） |
| benchmarking_mixed_precision | 实验 | 2 | autocast 下各张量 dtype 分析 + BF16 基准 |
| memory_profiling | 实验 | 4 | memory_viz 时间线、峰值显存、残差(saved tensors)分析 |
| gradient_checkpointing | 书面+实验 | 4 | 最优检查点策略（O(√N)/递归 O(log N)）+ 实测 |
| pytorch_attention | 实验 | 2 | 朴素注意力显存/耗时基准（何时 OOM） |
| torch_compile | 实验 | 2 | torch.compile 前后对比（注意力 + 整模型） |
| flash_benchmarking | 实验 | 5 | `triton.testing.do_bench` 对比 FA2 vs 朴素实现 |
| naive_ddp_benchmarking | 实验 | 3 | 朴素 DDP 每步耗时与通信占比（2 GPU, xl） |
| ddp_overlap_individual_parameters_benchmarking | 实验 | 1 | 重叠 vs 不重叠的 nsys 对比截图 |
| optimizer_state_sharding_accounting | 实验+书面 | 5 | 分片前后显存分解、耗时、与 ZeRO-1 差异 |
| fsdp_accounting | 实验+书面 | 5 | FSDP 显存节省预估 + all-gather 时序验证 |
| alternate_ring_all_reduce | 书面 | 1 | 备选环形 all-reduce 的耗时推导 |
| data_parallel_calcs | 书面 | 3 | DP 何时被通信瓶颈（FLOPs vs 通信时间） |
| fsdp_calcs | 书面 | 3 | FSDP 前向/反向的通信瓶颈推导 |
| tp_calcs | 书面 | 4 | 张量并行前向/反向 + 通信瓶颈推导 |
| fsdp_tp_calcs | 书面 | 6 | 2D 并行（FSDP×TP）最优组合推导 |
| leaderboard | 实验 | 10 | 8B 模型单步训练提速（2×B200，基线 10s） |
