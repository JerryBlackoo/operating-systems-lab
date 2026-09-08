# 01 · 端到端基准测试脚本（benchmarking_script）

讲义 §2.1.3 ｜ 4 分 ｜ 无 pytest（交付物是脚本，是后续所有 profiling 的基础设施）

```text
Problem:
benchmarking_script — 计时 forward / backward / optimizer step 的脚本

输入:
命令行超参:
- 模型尺寸 (Table 1): small(768/3072/12L/12H) medium(1024/4096/24L/16H)
  large(1280/5120/36L/20H) xl(2560/10240/32L/32H) 10B(4608/12288/50L/36H)
- vocab_size=10000, batch_size=4, context_length 默认 512
- w: warmup 步数；n: 测量步数
- 模式: forward-only / forward+backward / full step(含 optimizer)
- 开关: mixed precision (bf16) / memory profiling

输出:
每配置的平均耗时 + 标准差（建议自动生成表格）

参数:
无（随机权重 + 随机数据即可，只测速度/显存）

公式:
CUDA 调用是异步的: kernel 入队后 Python 立即返回
=> 必须 torch.cuda.synchronize() 后再读表，否则测的是 CPU 调度时间

步骤:
1. 按超参构建模型，生成随机 batch（torch.randint）
2. 跑 w 次 warmup（不计时；触发编译/内存分配/自动调优）
3. 计时 n 步: start=timeit.default_timer() -> 步骤 -> synchronize() -> stop
4. 记录每步耗时，输出 mean ± std
5. 可选: torch.autocast(device_type="cuda", dtype=torch.bfloat16) 包住前向
6. 可选: torch.cuda.memory._record_memory_history(max_entries=1000000)
   -> 跑目标阶段 -> _dump_snapshot("memory_snapshot.pickle")
   -> 拖进 pytorch.org/memory_viz 查看

需要查的 API:
- timeit.default_timer（最高精度时钟，优于 time.time）
- torch.cuda.synchronize()
- torch.autocast(device_type=, dtype=) / contextlib.nullcontext（无缝开关）
- torch.cuda.memory._record_memory_history / _dump_snapshot
- torch.cuda.nvtx: nvtx.range("name")（给 nsys 打标签、过滤 warmup）
- argparse

边界情况:
- 不 synchronize -> 测出来快得离谱（假的）
- 不 warmup -> 首几步含 CUDA context 初始化/编译/缓存分配，显著偏慢
- 只做 1-2 步 warmup 仍可能偏慢（多级缓存/自动调优未收敛），建议 ≥5 步
- backward 计时要含 loss 计算；full step 含 optimizer.step + zero_grad
- 大模型 + 长上下文会 OOM，脚本要能优雅报告

测试:
- 无 pytest；自检:
- 同配置多次运行 std 是否小？
- forward:backward 耗时比 ≈ 1:2（FLOPs 比）？
- 后续 nsys_profile / memory_profiling / mixed precision 实验都复用此脚本
```
