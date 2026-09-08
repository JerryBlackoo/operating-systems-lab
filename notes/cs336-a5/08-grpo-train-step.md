# 08 · GRPO 单步训练（grpo_train_step）

讲义 §4.2.4 / §5.4 / §6.4 ｜ 覆盖 3 个 Problem: `grpo_train_step_standard_on_policy`(5 分) / `_variants_on_policy`(2.5 分) / `_off_policy`(2.5 分) ｜ adapter: `run_grpo_train_step` ｜ 测试: `uv run pytest -k test_grpo_train_step_standard_on_policy` / `_variants_on_policy` / `_off_policy`

```text
Problem:
grpo_train_step — 给定一批 rollouts，完成一次完整的策略梯度更新
（含梯度累积、梯度裁剪、全部算法变体开关）

输入:
model / tokenizer / optimizer
gradient_accumulation_steps: int         微批数
max_grad_norm: float | None              step 前的梯度范数裁剪
reward_fn, repeated_prompts, rollout_responses, repeated_ground_truths
group_size: int
baseline / advantage_eps / advantage_normalizer      （卡 05 的开关）
importance_reweighting_method / old_log_probs / cliprange （卡 06 的开关）
loss_normalization / normalization_constant           （卡 07 的开关）

输出:
(loss, metadata)   —— loss 为整批损失（供日志），metadata 至少含:
loss、梯度范数、token entropy、train rewards (total/format)

参数:
算法配置速查（§5.4 / §6.4 实验用）:
  GRPO 标准:     mean + std + none + sequence
  GRPO_constant: mean + std + none + constant
  Dr_GRPO:       mean + none + none + constant
  RFT:           none + none + none + constant（只学正确样本）
  MaxRL:         mean + mean + none + constant
  offpolicy_*:   上述任一 + noclip/grpo(ε=0.2)/gspo(ε=3e-4)

公式:
整体流程 = 卡04(奖励) -> 卡05(优势) -> 卡02(分词) ->
           微批循环[卡03(logprob) -> 卡06(逐token损失) -> 卡07(聚合)
           -> backward] -> 裁剪 -> step

步骤:
1. raw_rewards, meta = compute_rollout_rewards(...)
2. advantages, _ = compute_group_normalized_rewards(...)
3. batch = tokenize_prompt_and_output(repeated_prompts, rollout_responses)
4. （variants）剪掉零优势序列: baseline=mean 时全组同奖励的组、
   baseline=none 时奖励为 0 的样本——它们梯度为 0，不必过模型；
   按同比例减少 grad accum 步数（微批大小不变）
5. for 每个微批:
   logp = get_response_log_probs(model, input_ids, labels)["log_probs"]
   per_tok, meta = compute_policy_gradient_loss(adv, logp, method,
                     old_logp_microbatch, cliprange, mask)
   loss = aggregate_loss_across_microbatch(per_tok, mask, ...)
   sequence 归一化时: loss *= n_micro / n_total   # 累积等价性
   loss.backward()
6. grad_norm = clip_grad_norm_(model.parameters(), max_grad_norm)
7. optimizer.step()；optimizer.zero_grad()
8. metadata: loss / grad_norm / token_entropy / rewards（+ 可选 pass@k）

需要查的 API:
- torch.nn.utils.clip_grad_norm_（返回裁剪前范数，正好拿去日志）
- optimizer.step / zero_grad
- 微批切片（advantages、mask、old_log_probs 都要跟着切！）

边界情况:
- 梯度累积归一化（最大坑）:
  sequence -> 微批 loss × (微批大小/全批大小)；constant -> 直接加
- 零优势剪枝必须保证与不剪枝"梯度完全等价"（constant 归一化下剪枝
  安全；sequence 归一化下剪序列会改变分母——注意讲义变体都用 constant）
- off-policy: old_log_probs 在 rollout 后用 no_grad 一次性算好，
  微批时按行切片对齐
- backward 在微批循环内、step/zero_grad 在循环外各一次
- RFT 只有正确样本有梯度: 直接不喂错误样本，大幅提速
- 熵/奖励等日志量要 detach，避免显存泄漏

测试:
- uv run pytest -k test_grpo_train_step_standard_on_policy
- uv run pytest -k test_grpo_train_step_variants_on_policy
- uv run pytest -k test_grpo_train_step_off_policy
- 自检: grad accum = 1 与 = 4（同一批数据）的最终梯度应一致
```
