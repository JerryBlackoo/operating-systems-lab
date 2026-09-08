# CS336 Assignment 5 (alignment) — 编程题速学卡片

来源: CS336 Spring 2026, Assignment 5: Reasoning RL (v26.0.0)。
配套仓库: [stanford-cs336/assignment5-alignment](https://github.com/stanford-cs336/assignment5-alignment)。

## 全局背景

- 任务：用 GRPO（及其变体）在 GSM8K 数学题上做 RL，提升 OLMo-2-0425-1B 的推理准确率。
- 基建都已提供：vLLM 推理与权重同步（`vllm_utils.py`）、数学答案评分器（`drgrpo_grader.py`）、prompt 文件（`prompts/`）。模型直接用 HuggingFace `transformers` 加载。
- 核心公式：REINFORCE 策略梯度 \(\hat g = \mathbb{E}[A \cdot \nabla_\theta \log \pi_\theta(y|x)]\)；GRPO 的 A = 组内 (reward − mean)/std；离策略时加重要性权重 \(w_t = \pi_\theta/\pi_0\) 并裁剪。
- 双 GPU 架构：GPU0 训练（HF 模型 + AdamW），GPU1 推理（vLLM），每次采样前同步权重。
- 一个函数多个变体 Problem 的（组归一化、损失、聚合、train step），合并为一张卡并标注全部测试。

## 编程题卡片索引（9 张，覆盖 15 个编程 Problem）

| # | 卡片 | Problem(s) | 分值 | 测试 |
|---|------|-----------|------|------|
| 01 | [prompting_baselines](01-prompting-baselines.md) | prompting_baselines | 5 | 无 pytest（评测报告） |
| 02 | [tokenize_prompt_and_output](02-tokenize-prompt-and-output.md) | tokenize_prompt_and_output | 1 | `-k test_tokenize_prompt_and_output` |
| 03 | [get_response_log_probs](03-get-response-log-probs.md) | get_response_log_probs | 1 | `-k test_get_response_log_probs` |
| 04 | [compute_rollout_rewards](04-compute-rollout-rewards.md) | compute_rollout_rewards | 1 | `-k compute_rollout_rewards` |
| 05 | [compute_group_normalized_rewards](05-compute-group-normalized-rewards.md) | …_grpo / …_drgrpo / …_maxrl | 1+0.5+0.5 | `-k compute_group_normalized_rewards_grpo` 等 |
| 06 | [compute_policy_gradient_loss](06-compute-policy-gradient-loss.md) | …_on_policy / …_off_policy / …_off_policy_gspo | 1+1+1 | `-k test_compute_policy_gradient_loss_on_policy` 等 |
| 07 | [aggregate_loss_across_microbatch](07-aggregate-loss.md) | …_sequence / …_constant | 0.5+0.5 | `-k test_aggregate_loss_across_microbatch_sequence` 等 |
| 08 | [grpo_train_step](08-grpo-train-step.md) | …_standard_on_policy / …_variants_on_policy / …_off_policy | 5+2.5+2.5 | `-k test_grpo_train_step_standard_on_policy` 等 |
| 09 | [grpo_training_loop](09-grpo-training-loop.md) | grpo_experiments_standard_on_policy(a) | 10 | 无 pytest（50 步冒烟 + 4 seed） |

依赖链：02/03 是分词与打分原语；04→05 产出优势；06→07 产出损失；08 组装成单步更新；09 加上 vLLM 采样闭环。变体开关速查表见卡 08。

## 非编程题清单（书面推导 / 实验）

| Problem | 类型 | 分值 | 内容 |
|---------|------|------|------|
| baseline_calcs | 书面 | 5 | 二值策略下策略梯度估计量的方差推导（baseline 何时降方差） |
| think_about_length_normalization | 书面 | 1 | 序列长度归一化 vs 常数归一化的利弊 |
| think_about_rft | 书面 | 2 | RFT 与 Dr.GRPO 目标的期望/方差对比 |
| derive_difficulty_reweightings | 书面 | 6 | 三种优势归一化诱导的难度重加权 w(x) 推导 |
| think_about_advantage_normalization | 书面 | 2 | 除 std / 除 mean / 不除的利弊 |
| derive_surrogate_objectives | 书面 | 2 | "成对"重要性加权的代理目标推导 |
| think_about_importance_reweighting | 书面 | 2 | none / token 级裁剪 / GSPO 的偏差-方差谱系 |
| grpo_learning_rate | 实验 | 3 | 学习率扫描（4 B200 hrs） |
| grpo_prompt_ablation | 实验 | 3 | question_only vs r1_zero vs 3-shot 的 RL 动态 |
| grpo_experiments_variants_on_policy | 实验 | 10 | GRPO_constant / Dr_GRPO / RFT / MaxRL 对比（8 B200 hrs） |
| grpo_experiments_off_policy | 实验 | 10 | 32x off-policy: naive / noclip / clip(0.2) / gspo(3e-4)，记录 clip fraction |
| try_your_own | 实验 | 10 | 自创策略梯度估计量（只改一处）并与基线对比 |
