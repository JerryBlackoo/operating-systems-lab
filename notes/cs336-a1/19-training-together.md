# 19 · 训练主循环（training_together）

讲义 §5.3 ｜ 4 分 ｜ 无 pytest（交付物是可运行的训练脚本）

```text
Problem:
training script — 把 tokenizer 产物、模型、优化器、数据加载、checkpoint 串成主循环

输入:
命令行参数（argparse），至少支持:
- 模型超参: vocab_size, context_length, num_layers, d_model, num_heads, d_ff, rope_theta
- 优化超参: lr(α_max/α_min), warmup_iters, cosine_cycle_iters, betas, eps,
  weight_decay, grad_clip_norm, batch_size, total_iters
- 路径: 训练/验证 token 数据(.npy), checkpoint 保存/恢复路径
- 日志: 打印/评估间隔, wandb 开关, device

输出:
训练好的 checkpoint + 训练/验证 loss 日志（含 step 与 wall-clock time）

参数:
（脚本本身无参数，组织已有组件）

公式:
每步: batch -> logits -> cross_entropy -> backward -> clip -> 设 lr -> step
TinyStories 参考配置: vocab 10000, ctx 256, d_model 512, d_ff 1344,
4 层, 16 头, Θ=10000, 总 token 数 ≈ 327,680,000 = B × steps × ctx

步骤:
1. argparse 解析全部超参
2. train/val 数据 np.load(..., mmap_mode='r')
3. 构建 TransformerLM 与 AdamW；若指定 resume 则 load_checkpoint 拿回 iteration
4. 循环 t = start..total_iters:
   a. inputs, targets = get_batch(train_data, B, ctx, device)
   b. loss = cross_entropy(model(inputs), targets)
   c. optimizer.zero_grad(); loss.backward()
   d. gradient_clipping(model.parameters(), M)
   e. lr_t = lr_cosine_schedule(t, ...)；写入每个 param_group["lr"]
   f. optimizer.step()
5. 每隔 K 步: torch.no_grad() 下算验证 loss；记录 step + wall-clock；打日志/wandb
6. 每隔 N 步: save_checkpoint(model, opt, t, path)

需要查的 API:
- argparse.ArgumentParser
- optimizer.param_groups（每步覆写 "lr"）
- optimizer.zero_grad(set_to_none=True)
- torch.no_grad() / model.eval() / model.train()
- time.time()（wall-clock 记录，实验交付要求）
- wandb.init / wandb.log（可选）
- torch.compile（低资源提速: cpu 直接 compile；mps 用 backend="aot_eager"）

边界情况:
- 顺序别乱: backward -> clip -> 设 lr -> step -> zero_grad
- 学习率每步都要写进 param_groups，否则 schedule 不生效
- 验证时切 no_grad（省显存防泄漏），验证完切回训练
- memmap 模式打开数据，否则大数据集 OOM
- 恢复训练时 iteration/schedule/optimizer 状态要一起接上
- cosine 退火终点 T_c 设为总步数（讲义建议）

测试:
- 无 pytest；自检手段:
- 单 batch 过拟合: 反复用同一个 batch 训练，loss 应能逼近 0（架构正确性）
- loss 曲线: TinyStories 上验证 loss 应能到 ≤1.45（GPU 配置）或 ≤2.00（CPU/MPS）
- 中断 -> resume 后曲线无缝衔接？
- 监控激活/梯度范数不爆不消
```
