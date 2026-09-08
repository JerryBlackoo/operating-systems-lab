# 09 · GRPO 完整训练循环（grpo_experiments_standard_on_policy）

讲义 §4.3 ｜ 10 分（含实验）｜ 无 pytest（交付物是训练脚本 + 4 seed 实验）

```text
Problem:
GRPO 训练循环 — 串起 vLLM 采样与策略训练，在 GSM8K 上提升
OLMo-2-0425-1B（目标: 最终验证准确率 ≥ 25%，4 seed 平均）

输入:
命令行参数: 模型名、prompt 类型、训练/验证集路径、采样超参、训练超参

输出:
训练脚本 + wandb 曲线（loss / grad norm / token entropy /
train & val rewards / val 平均回复长度）+ 训练前后 rollout 样例

参数:
参考超参（讲义 §4.3，直接照抄起步）:
  n_train_examples=6400, n_val_examples=1024
  num_rollout_steps=200, learning_rate=1e-5
  rollout_batch_size = train_batch_size = 256   # 计的是"回复"数
    = 32 prompts × group_size 8
  gradient_accumulation_steps=32
  sampling: temperature=1.0, top_p=1.0, max_tokens=512,
            stop=["</answer>"], include_stop_str_in_output=True
  max_grad_norm=1.0
  AdamW(lr=1e-5, betas=(0.9, 0.95), weight_decay=0.0)
  双卡分工: GPU0 = 训练(HF 模型+优化器)，GPU1 = vLLM 推理

公式:
循环不变式: 每个 rollout batch 前，vLLM 权重 = 当前策略权重
on-policy: 每个推理批只做 1 次梯度更新（train_batch = rollout_batch）

步骤:
1. 初始化: 加载 GSM8K jsonl；get_model_and_tokenizer(model_id, "cuda:0")
   （bfloat16 + flash_attention_2）；AdamW；wandb.init
2. server = VLLMServer(gpu=1)；server.start()；
   server.init_weight_sync(policy_device)
3. 每个 rollout step:
   a. server.sync_policy_weights(policy)       # 关键! 先同步再采样
   b. 采样 32 个问题，每题填入 r1_zero prompt，
      generate_completions 出 8 条回复/题（共 256 条）
   c. （off-policy 时: no_grad 算 old_log_probs）
   d. grpo_train_step(...)                     # 卡 08
   e. 每 10 个 rollout batch: 验证 ≥1024 条（准确率+平均回复长度）
   f. 每 40 个 rollout batch: 落盘当前 rollouts 供人工检查
4. 保存 model.save_pretrained / tokenizer.save_pretrained
5. 跑 4 个随机 seed，画 mean±区间 曲线（不能只画均值）

需要查的 API:
- vllm_utils.VLLMServer: start / init_weight_sync / sync_policy_weights /
  generate_completions
- checkpoint.get_model_and_tokenizer（bfloat16 + FA2）
- wandb.init / log；model.save_pretrained
- torch.no_grad()（验证与 old_log_probs）

边界情况:
- 每次采样前必须 sync 权重，否则 rollout 来自旧策略而你按 on-policy 训练
- reward 起步接近 0 是"正常的"——采到第一条正确 rollout 前学不到东西，
  不同 seed 起飞时间差异很大
- 验证必须 ≥1024 条: CoT/RL 评测噪声很大，样本少了曲线没法看
- prompt 用 zero-shot r1_zero；GT 从 answer 里 split("####") 提取
- RL 方差极大: 结论必须基于多 seed + 方差可视化（mean±1.96σ/√n 或
  min/max）
- 显存: 训练侧 grad accum 32（每微批 8 条回复）
- 日志里记录 rollout 文本，肉眼确认推理链质量在变好

测试:
- 无 pytest；自检:
- 先跑 ~50 步冒烟: val reward 是否上升、rollout 是否越来越像样
- 完整跑: 4 seeds 平均最终 val accuracy ≥ 25%（交付线）
- token entropy 通常随训练下降（策略变确定）；回复长度变化值得观察
```
