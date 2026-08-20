# CS336 Assignment 1 (basics) — 编程题速学卡片

来源: CS336 Spring 2026, Assignment 1: Building a Transformer LM (v26.0.3)。
每道**编程题**一张“手写纸”：输入/输出/参数/公式/步骤/需要查的 API/边界情况/测试。
配套代码仓库: [stanford-cs336/assignment1-basics](https://github.com/stanford-cs336/assignment1-basics)。

## 全局约束（做题前先记住）

- 禁用 `torch.nn`、`torch.nn.functional`、`torch.optim` 中的一切定义，**除了**：`nn.Parameter`、容器类（`Module` / `ModuleList` / `Sequential` 等）、`torch.optim.Optimizer` 基类。
- 所有测试通过 `adapters.py` 里的胶水函数调用你的实现：`uv run pytest`。
- 建议全程用 einops 的 `einsum` / `rearrange`；批量维一律放最前（`(..., d)`）。
- 初始化约定: Linear `N(0, 2/(din+dout))` 截断 ±3σ；Embedding `N(0,1)` 截断 ±3；RMSNorm 全 1。

## 编程题卡片索引（20 张）

| # | 卡片 | Problem | 分值 | 讲义 | 测试 |
|---|------|---------|------|------|------|
| 01 | [train_bpe](01-train-bpe.md) | BPE 分词器训练 | 15 | §2.5 | `uv run pytest tests/test_train_bpe.py` |
| 02 | [tokenizer](02-tokenizer.md) | 分词器编解码类 | 15 | §2.6 | `uv run pytest tests/test_tokenizer.py` |
| 03 | [linear](03-linear.md) | 线性层 | 1 | §3.3.2 | `uv run pytest -k test_linear` |
| 04 | [embedding](04-embedding.md) | 词嵌入层 | 1 | §3.3.3 | `uv run pytest -k test_embedding` |
| 05 | [rmsnorm](05-rmsnorm.md) | RMSNorm | 1 | §3.4.1 | `uv run pytest -k test_rmsnorm` |
| 06 | [positionwise_feedforward](06-positionwise-feedforward.md) | SwiGLU FFN | 2 | §3.4.2 | `uv run pytest -k test_swiglu` |
| 07 | [rope](07-rope.md) | 旋转位置编码 | 2 | §3.4.3 | `uv run pytest -k test_rope` |
| 08 | [softmax](08-softmax.md) | Softmax | 1 | §3.4.4 | `uv run pytest -k test_softmax_matches_pytorch` |
| 09 | [scaled_dot_product_attention](09-scaled-dot-product-attention.md) | 缩放点积注意力 | 5 | §3.4.4 | `uv run pytest -k test_scaled_dot_product_attention` |
| 10 | [multihead_self_attention](10-multihead-self-attention.md) | 因果多头自注意力 | 5 | §3.4.5 | `uv run pytest -k test_multihead_self_attention` |
| 11 | [transformer_block](11-transformer-block.md) | Transformer 块 | 3 | §3.5 | `uv run pytest -k test_transformer_block` |
| 12 | [transformer_lm](12-transformer-lm.md) | 完整 Transformer LM | 3 | §3.5 | `uv run pytest -k test_transformer_lm` |
| 13 | [cross_entropy](13-cross-entropy.md) | 交叉熵损失 | 1 | §4.1 | `uv run pytest -k test_cross_entropy` |
| 14 | [adamw](14-adamw.md) | AdamW 优化器 | 2 | §4.3 | `uv run pytest -k test_adamw` |
| 15 | [learning_rate_schedule](15-learning-rate-schedule.md) | 余弦退火调度 | 1 | §4.4 | `uv run pytest -k test_get_lr_cosine_schedule` |
| 16 | [gradient_clipping](16-gradient-clipping.md) | 梯度裁剪 | 1 | §4.5 | `uv run pytest -k test_gradient_clipping` |
| 17 | [data_loading](17-data-loading.md) | 数据加载 get_batch | 2 | §5.1 | `uv run pytest -k test_get_batch` |
| 18 | [checkpointing](18-checkpointing.md) | 检查点存取 | 1 | §5.2 | `uv run pytest -k test_checkpointing` |
| 19 | [training_together](19-training-together.md) | 训练主循环脚本 | 4 | §5.3 | 无 pytest（单 batch 过拟合自检） |
| 20 | [decoding](20-decoding.md) | 文本生成/解码 | 3 | §6 | 无 pytest（生成样例自检) |

推荐实现顺序即卡片顺序：01→20 正好是讲义依赖链（tokenizer → 模型积木 → 损失/优化器 → 训练 → 生成）。

## 非编程题清单（书面 / 实验，不做卡片）

| Problem | 类型 | 分值 | 内容 |
|---------|------|------|------|
| unicode1 / unicode2 | 书面 | 1+3 | Unicode 与 UTF-8 概念题 |
| train_bpe_tinystories / train_bpe_expts_owt | 实验 | 2+2 | 在 TinyStories / OWT 上训练 BPE，分析最长 token 与耗时 |
| tokenizer_experiments | 实验 | 4 | 压缩率、吞吐、用 uint16 序列化数据集 |
| transformer_accounting | 书面 | 5 | GPT-2 系列参数量 / FLOPs 核算（矩阵乘 2mnp 规则） |
| learning_rate_tuning | 小实验 | 1 | 玩具 SGD 上试 lr=1e1/1e2/1e3 观察发散 |
| adamw_accounting | 书面 | 2 | AdamW 峰值显存与 FLOPs 核算、MFU 估算 |
| experiment_log | 实验 | 3 | 实验记录基础设施（loss 曲线 × step/wall-clock） |
| learning_rate / batch_size_experiment | 实验 | 3+1 | lr 扫描（目标 val loss ≤1.45）、batch size 影响 |
| generate | 实验 | 1 | 用训练好的模型生成 ≥256 token 并点评 |
| layer_norm_ablation / pre_norm_ablation | 消融 | 1+1 | 去掉 RMSNorm / 改回 post-norm |
| no_pos_emb / swiglu_ablation | 消融 | 1+1 | NoPE vs RoPE / SiLU(d_ff=4d) vs SwiGLU |
| main_experiment | 实验 | 2 | 相同架构在 OpenWebText 上训练并对比 |
| leaderboard | 实验 | 6 | 45 分钟 B200 预算内最小化 OWT 验证 loss（基线 5.0） |
