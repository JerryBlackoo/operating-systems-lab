# 05 · 组归一化优势（compute_group_normalized_rewards）

讲义 §4.1.5–4.1.6 / §5.1 / §5.3 ｜ 覆盖 3 个 Problem: `compute_group_normalized_rewards_grpo`(1 分) / `_drgrpo`(0.5 分) / `_maxrl`(0.5 分) ｜ adapter: `run_compute_group_normalized_rewards` ｜ 测试: `uv run pytest -k compute_group_normalized_rewards_grpo` / `_drgrpo` / `_maxrl`

```text
Problem:
compute_group_normalized_rewards — 把原始奖励变成优势 (advantage)
一个函数支持三种算法: GRPO / Dr.GRPO / MaxRL

输入:
raw_rewards: (rollout_batch_size,)   = (n_prompts × group_size,)
group_size: int
baseline: "mean" | "none"                 是否减组均值
advantage_eps: float = 1e-6               除法防零
advantage_normalizer: "std" | "none" | "mean"   除以什么

输出:
advantages: (rollout_batch_size,)
metadata: dict（组奖励的 mean/std/max/min 等）

参数:
无

公式:
先 reshape 成 (n_prompts, group_size)，按组计算:
μ_i   = mean(组内奖励)
std_i = torch.std(组内奖励)        # 注意! 用 torch.std 默认(带 Bessel 校正)
A = r − μ_i        (baseline="mean")     或 A = r  (baseline="none")
再除:
  "std":  A / (std_i + eps)      -> 标准 GRPO
  "none": A                      -> Dr. GRPO
  "mean": A / (μ_i + eps)        -> MaxRL（难题上调权重: μ 小 -> 权重大）

三种算法对应配置:
  GRPO:    baseline=mean, normalizer=std
  Dr.GRPO: baseline=mean, normalizer=none
  MaxRL:   baseline=mean, normalizer=mean
  RFT:     baseline=none, normalizer=none（只在 train step 里配合指示函数）

步骤:
1. grouped = raw_rewards.view(-1, group_size)
2. mu = grouped.mean(dim=1, keepdim=True)
   std = grouped.std(dim=1, keepdim=True)     # 默认 unbiased=True，别改
3. 按 baseline 减 μ；按 normalizer 除 (std+eps) / 不除 / 除 (μ+eps)
4. advantages = result.view(-1)；metadata 记组统计
5. 不支持的组合 raise NotImplementedError（讲义允许）

需要查的 API:
- Tensor.view(-1, group_size) / Tensor.mean(dim, keepdim)
- torch.std（确认默认 correction=1，讲义明确要求用默认）
- keepdim 广播

边界情况:
- torch.std 的 Bessel 校正: 讲义公式写的是 1/G，但明确说"用 torch.std
  默认值"——测试按默认对拍，别自己实现 1/G 版本
- 全组同奖励 -> std=0 -> 必须 +eps 防除零（此时 A 也为 0，天然零梯度）
- MaxRL: μ=0（全错组）时 A=0/eps=0，安全
- reshape 的前提: raw_rewards 排列是"同组连续"（rollout 展平顺序）
- 除 std 后估计量已不再无偏——这是稳定性 trick（考点）

测试:
- uv run pytest -k compute_group_normalized_rewards_grpo（std）
- uv run pytest -k compute_group_normalized_rewards_drgrpo（none + baseline none）
- uv run pytest -k compute_group_normalized_rewards_maxrl（mean）
- 手算: 组 [1,0,0,1], μ=0.5, torch.std≈0.577 -> A≈[0.866,-0.866,...]
```
