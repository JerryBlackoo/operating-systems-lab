# 04 · 计算 rollout 奖励（compute_rollout_rewards）

讲义 §4.2.3 ｜ 1 分 ｜ adapter: `run_compute_rollout_rewards` ｜ 测试: `uv run pytest -k compute_rollout_rewards`

```text
Problem:
compute_rollout_rewards — 用奖励函数给一批 rollout 打分

输入:
reward_fn: Callable[[str, str], dict[str, float]]
    (response, ground_truth) -> {"reward", "format_reward", "answer_reward"}
rollout_responses: list[str]        长度 = rollout_batch_size
                                    = n_prompts_per_rollout_batch × group_size
repeated_ground_truths: list[str]   同长度（每个 GT 重复 group_size 次）

输出:
raw_rewards: torch.Tensor (rollout_batch_size,)   每条 rollout 的总奖励
metadata: dict[str, float]   至少含 batch 平均 total reward 和 format reward

参数:
无

公式:
raw_rewards[k] = reward_fn(rollout_responses[k], repeated_ground_truths[k])["reward"]
本作业无部分分: total reward = answer reward（format 只用于日志）

步骤:
1. 逐条调用 reward_fn（zip 两个列表）
2. 收集 "reward" 到张量；顺带累计 format_reward / answer_reward
3. metadata = {"mean_reward": ..., "mean_format_reward": ...}（可再加
   max/min、正确率等）
4. 返回 (raw_rewards, metadata)

需要查的 API:
- torch.tensor(list, dtype=torch.float32)
- （评分器）cs336_alignment.drgrpo_grader.r1_zero_reward_fn

边界情况:
- 两个列表必须一一对齐: GT 是"按 prompt 重复 group_size 次"后的展平序
  （顺序错了整个 RL 都在拿错奖励学习）
- reward_fn 可能较慢（符号数学解析），必要时可并行，但注意顺序
- 奖励是 0/1 二值（本作业），但代码不要写死，按 float 处理
- metadata 只放标量（要进 wandb）

测试:
- uv run pytest -k compute_rollout_rewards
- 手算: 3 条 rollout 奖励 [1, 0, 1] -> mean_reward ≈ 0.667
- raw_rewards.shape == (len(rollout_responses),)？
```
