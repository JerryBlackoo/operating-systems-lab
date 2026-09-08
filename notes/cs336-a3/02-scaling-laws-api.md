# 02 · 用训练 API 构建缩放定律（scaling_laws）

讲义 §3 ｜ 50 分 ｜ 无 pytest（交付物: writeup + API 最终提交，占本作业绝大部分分值）

```text
Problem:
scaling_laws — 12 B200 小时实验预算内拟合缩放定律，
预测 48 B200 小时预算下最优模型配置与最终验证 loss

输入:
训练 API（须在 Stanford 网络内，X-API-Key: 学号）:
  POST /submit             提交实验（排队，按 max_runtime_seconds 预留预算）
  GET  /budget             查已用/剩余秒数
  GET  /experiments        列出所有实验
  GET  /experiment/{id}    轮询单个实验（queued/running/completed/failed）
  POST /final_submission   提交最终预测（可覆盖重交）
submit 的 body:
  architecture_config: attention_bias, head_dim, hidden_size,
    intermediate_size, num_attention_heads, num_hidden_layers,
    num_key_value_heads, rms_norm_eps, rope_theta, tie_word_embeddings,
    dtype("float32"/"bfloat16"), vocab_size(=32000)
  optimizer_config: lr_scheduler{peak_value, final_lr_frac, warmup_frac,
    init_value}, weight_decay, beta1, beta2, eps, eps_root, grad_clip_norm
  train_batch_size / val_batch_size / n_evals / total_train_tokens /
  max_runtime_seconds / model_seed

输出:
(1) 预测最优超参配置 (2) 预测验证 loss (3) 可复现的 writeup

参数:
固定: seq_len=512, 验证集 2^18 tokens, DCLM 数据 ≤500B tokens, 数据顺序固定
预算: 拟合 12 B200 小时（API 硬性拒绝超额），目标 48 B200 小时

公式:
C ≈ 6·N·D（FLOPs = 6 × 参数量 × token 数）
非嵌入参数估计: N ≈ 12 · n_layer · d_model²
IsoFLOPs 法: 每个预算档 C_i 扫模型大小 -> loss-N 呈 U 形 ->
  N_opt(C_i) 拟幂律 N_opt ∝ C^a 外推
（也可用 Kaplan 方法或 loss(C) 幂律直接预测最终 loss）

步骤:
1. 写 API client: submit / poll / budget，所有响应落盘缓存
   （json 文件），避免重复消耗预算
2. 用最小配置（如 30s 的 run）验证全流程，再开始正式实验
3. 小规模先定超参: 扫 lr、batch size（loss 对 lr 最敏感），
   选每档稳定的设置；lr 可随模型尺寸按经验缩放
4. 设计 IsoFLOPs 网格: 选 3~5 个 wall-clock 档位（如 10min/30min/1h...），
   每档跑 4~6 个不同模型尺寸（改 n_layer/d_model，保持形状合理）
5. 每档取最低 loss -> (C_i, N_opt, D_opt, L_opt)；log-log 拟合
   N_opt∝C^a、D_opt∝C^b、L_opt vs C 幂律
6. 外推到 48 B200 小时对应的 C；用 N ≈ 12·n_layer·d_model² 反解
   模型形状（同时给出 lr/batch 等配套超参与 total_train_tokens）
7. POST /final_submission 提交配置 + 预测 loss（可多次覆盖）

需要查的 API:
- requests.get / requests.post(headers={"X-API-Key": ...}, json=...)
- scipy.optimize.curve_fit / np.polyfit（log 空间）
- json 缓存、matplotlib 画 IsoFLOPs 曲线
- （参考）Hoffmann et al. 2022 (Chinchilla)、Kaplan et al. 2020

边界情况:
- 预算规则: 排队/运行即按 max_runtime_seconds 预留；完成后按实际耗时
  退差额；超时按满额扣 => max_runtime 要"略大于预估"，不能拍脑袋 12h
- 相同配置重复提交返回 409 且不再扣预算（可安全重试）
- timeout 的 run 只有 partial_val_losses，不算完成 run
- model_seed 只影响初始化，数据顺序永远一样
- 约束: hidden_size = num_attention_heads × head_dim；
  num_attention_heads % num_key_value_heads == 0；
  total_train_tokens % (512 × train_batch_size) == 0
- 花钱前先纸上规划全部实验矩阵！12h 预算 = 只有一次机会的实验设计

测试:
- 无 pytest；自检:
- 每档 IsoFLOPs 曲线是否 U 形（不是的话档位/尺寸范围选错了）
- 拟合优度 R²、a+b≈1
- 预测配置代回 C=6ND 与 48h 预算是否自洽
- 最终提交前 GET /final_submission 确认已生效
```
