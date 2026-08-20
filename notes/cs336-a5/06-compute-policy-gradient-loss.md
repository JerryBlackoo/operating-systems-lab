# 06 · 策略梯度逐 token 损失（compute_policy_gradient_loss）

讲义 §4.2.3 / §6.2 / §6.3 ｜ 覆盖 3 个 Problem: `compute_policy_gradient_loss_on_policy`(1 分) / `_off_policy`(1 分) / `_off_policy_gspo`(1 分) ｜ adapter: `run_compute_policy_gradient_loss` ｜ 测试: `uv run pytest -k test_compute_policy_gradient_loss_on_policy` / `_off_policy` / `_off_policy_gspo`

```text
Problem:
compute_policy_gradient_loss — 计算逐 token 的策略梯度损失
一个函数支持 4 种重要性加权: none / noclip / grpo(PPO clip) / gspo

输入:
raw_rewards_or_advantages: (B,) 或 (B,1)      每条 rollout 一个标量 A
policy_log_probs: (B, S)                      当前策略的逐 token logprob
importance_reweighting_method: "none"|"noclip"|"grpo"|"gspo"
old_log_probs: (B, S) | None                  采样时旧策略的 logprob
                                              （method≠"none" 时必需）
cliprange: float | None                       ε（"grpo"/"gspo" 必需）
response_mask: (B, S) | None                  GSPO 求序列均值时必需

输出:
per_token_loss: (B, S)   （聚合交给下一张卡的函数）
metadata: dict           （如 clip fraction）

参数:
无

公式:
（实现时返回"负的"目标 J，让梯度下降=梯度上升）
A 广播成 (B,1)。令 w_t = exp(policy_lp − old_lp)   逐 token 重要性权重
1. none   (on-policy):  J_t = A · policy_log_probs_t
2. noclip (off-policy): J_t = A · w_t
   （∇(A·w_t) = A·w_t·∇log π，正是加权策略梯度）
3. grpo   (PPO 式 token 级裁剪):
   J_t = min( A·w_t , A·clip(w_t, 1−ε, 1+ε) )
4. gspo   (序列级几何平均):
   s = exp( mean_{t∈mask}(policy_lp − old_lp) )    每序列一个标量
   J_t = min( A·s , A·clip(s, 1−ε, 1+ε) )          广播回每个 token

步骤:
1. A = raw_rewards_or_advantages.view(-1, 1)
2. 按 method 分支实现上面公式；loss = −J
3. grpo/gspo: 顺带统计 clip fraction（min 取了 clip 分支的占比）
4. gspo 的均值只在 response_mask 内取（(diff*mask).sum / mask.sum）
5. 不支持的组合 raise NotImplementedError

需要查的 API:
- torch.exp / torch.clamp(w, 1-eps, 1+eps)
- torch.minimum（逐元素 min，注意不是 torch.min 的 reduce 形式）
- Tensor.detach()（old_log_probs 是常数，不回传梯度）
- masked 均值: (x*mask).sum(-1)/mask.sum(-1)

边界情况:
- 比值一律用 exp(logp 差)，不要 p/p（下溢）
- old_log_probs 必须 detach（它来自旧模型，本来就该是常数——
  训练脚本里通常已 no_grad 计算）
- clip 目标里的 min 是"逐元素"，且 A 可为负: min(A·w, A·clip(w)) 的
  写法自动处理正负 advantage 两种情形，别自己按符号分支写错
- GSPO 的 s 数值稳定: 先在 log 域求均值再 exp（几何平均）
- GSPO cliprange 量级和 GRPO 完全不同（推荐 3e-4 vs 0.2）
- 返回的是"未 mask、未聚合"的 (B,S)——mask 在聚合阶段应用

测试:
- uv run pytest -k test_compute_policy_gradient_loss_on_policy
- uv run pytest -k test_compute_policy_gradient_loss_off_policy
- uv run pytest -k test_compute_policy_gradient_loss_off_policy_gspo
- 手算: old=policy（w=1）时 noclip/grpo 应退化为 on-policy 情形的梯度
```
