# CS336 A1–A5 参考实现

按 `notes/cs336-a*/` 里的速学卡片实现的可运行代码，每个作业一个包，共用一套自测（`tests/`，103 个测试，无 GPU 即可全部通过）。

```bash
pip install torch --index-url https://download.pytorch.org/whl/cpu
pip install numpy regex einops pytest scipy matplotlib mmh3
cd cs336 && python -m pytest tests -q     # 103 passed
```

没有官方测试夹具，验证策略是：**对照 PyTorch 参考实现逐元素对拍**（Linear vs `F.linear`、softmax vs `torch.softmax`、SDPA vs `F.scaled_dot_product_attention`、AdamW vs `torch.optim.AdamW`、梯度裁剪 vs `clip_grad_norm_`…）+ **讲义手算例子**（BPE 合并顺序、`'the cat ate' -> [9,7,1,5,10,3]`、GRPO 组归一化数值…）+ **端到端不变量**（单 batch 过拟合到 loss<0.1、分布式训练与单进程逐参数一致、玩具 RL 任务奖励上升）。

## 包结构与卡片对应

| 包 | 作业 | 内容 | 测试 |
|----|------|------|------|
| `cs336_basics/` | A1 | `bpe.py` `tokenizer.py` `model.py` `optim.py` `data.py` `checkpoint.py` `decode.py` `train.py`（卡 01–20 全部） | `test_a1_*.py`，45 个 |
| `cs336_systems/` | A2 | `benchmarking.py` `flash_attention.py`（PyTorch 分块版 + Triton kernel）`distributed_benchmark.py` `ddp.py`（naive/flat/overlap）`sharded_optimizer.py` `fsdp.py` | `test_a2_systems.py`，15 个（gloo 双进程） |
| `cs336_scaling/` | A3 | `isoflops.py`（官方数据已内置）`api_client.py`（带缓存/dry-run） | `test_a3_scaling.py`，5 个 |
| `cs336_data/` | A4 | `extract.py` `classifiers.py` `pii.py` `gopher.py` `dedup.py`（exact + MinHash/LSH）`pipeline.py` `tokenize_data.py` | `test_a4_data.py`，18 个 |
| `cs336_alignment/` | A5 | `grpo.py`（7 个组件，全部变体开关）`grader.py` `prompts.py` `evaluate.py` `train_grpo.py` | `test_a5_grpo.py`，20 个（含 CPU 迷你 RL 收敛） |

## 可直接运行的入口

```bash
# A1: 在合成数据上训练一个小 LM（CPU 几秒钟，loss 4.5 -> 0.27）
python - <<'EOF'
import numpy as np
rng = np.random.default_rng(0)
pattern = rng.integers(0, 64, size=16)
data = np.tile(pattern, 5000).astype(np.uint16)
np.save("/tmp/train.npy", data[:70000]); np.save("/tmp/val.npy", data[70000:])
EOF
python -m cs336_basics.train --train-data /tmp/train.npy --val-data /tmp/val.npy \
    --vocab-size 64 --context-length 32 --d-model 64 --num-heads 4 --d-ff 128 \
    --num-layers 2 --batch-size 16 --total-iters 300 --warmup-iters 30 \
    --lr-max 1e-3 --lr-min 1e-4 --ckpt-path /tmp/lm.ckpt

# A2: 基准脚本 / all-reduce 基准（CPU 用 gloo）
python -m cs336_systems.benchmarking --size tiny --mode full --device cpu
python -m cs336_systems.distributed_benchmark --backend gloo --world-size 2 --sizes-mb 1 10

# A3: IsoFLOPs 拟合 + 外推（用官方 72 个 run 的数据）
python -m cs336_scaling.isoflops --data cs336_scaling/data/isoflops_curves.json --plot-dir /tmp/plots

# A4: 数据过滤管线（无分类器模型时用 --no-classifiers 只跑规则+PII+去重）
python -m cs336_data.pipeline --input-dir WET_DIR --output-dir OUT --no-classifiers
python -m cs336_data.tokenize_data --input filtered.txt --output tokens.bin  # 需 transformers

# A5: GRPO（组件在 CPU 全测；真实 OLMo 训练需 2 GPU + vllm + 课程 vllm_utils.py）
python -m cs336_alignment.train_grpo --model allenai/OLMo-2-0425-1B \
    --train-jsonl data/gsm8k/train.jsonl --val-jsonl data/gsm8k/test.jsonl
```

## 需要外部资源才能跑的部分（代码已就绪，均有守卫和文档）

- **A2 Triton kernel**（`FlashAttentionTriton`）：需要 CUDA GPU；无 GPU 时 PyTorch 版 `FlashAttentionPyTorch` 功能等价（测试即用它）。
- **A3 训练 API**：需要 Stanford 内网 + API key；`api_client.py --dry-run` 可离线查看请求。
- **A4 fastText/resiliparse 分类器**：需要下载 `lid.176.bin` 与 Dolma jigsaw 模型（路径可用环境变量 `CS336_LID_MODEL` 等覆盖）；纯规则部分（PII/Gopher/去重）无此依赖。
- **A5 真实 RL 训练**：需要 2 张 GPU、`vllm` 和官方仓库的 `vllm_utils.py`（权重同步实现属课程提供的基建）。

## 与官方测试对接

官方仓库（`stanford-cs336/assignment*`）通过 `tests/adapters.py` 调用实现。函数签名按讲义推荐接口实现，填 adapter 时直接转调即可，例如 A1 的 `run_rmsnorm -> cs336_basics.RMSNorm`、A5 的 `run_grpo_train_step -> cs336_alignment.grpo_train_step`。
