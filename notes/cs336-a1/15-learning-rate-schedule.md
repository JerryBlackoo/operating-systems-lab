# 15 · 余弦退火学习率调度（learning_rate_schedule）

讲义 §4.4 ｜ 1 分 ｜ adapter: `adapters.get_lr_cosine_schedule` ｜ 测试: `uv run pytest -k test_get_lr_cosine_schedule`

```text
Problem:
lr_cosine_schedule — 线性 warmup + 余弦退火（纯函数，无状态）

输入:
t: int          当前迭代步
alpha_max: float    最大学习率
alpha_min: float    最小（最终）学习率
T_w: int        warmup 迭代数
T_c: int        余弦退火结束的迭代步

输出:
alpha_t: float   第 t 步应使用的学习率

参数:
无

公式:
(warmup)      t < T_w:        α_t = (t / T_w) · α_max
(cosine)      T_w ≤ t ≤ T_c:  α_t = α_min + ½(1 + cos( (t−T_w)/(T_c−T_w) · π ))·(α_max − α_min)
(post)        t > T_c:        α_t = α_min

步骤:
1. if t < T_w: return t / T_w * alpha_max        # 注意浮点除法
2. elif t <= T_c: 按余弦公式返回
3. else: return alpha_min
4. 训练循环里每步调用，把结果写进 optimizer.param_groups 的 "lr"

需要查的 API:
- math.cos, math.pi
- （使用侧）for group in optimizer.param_groups: group["lr"] = alpha_t

边界情况:
- t = T_w: 属于余弦段，cos(0)=1 -> α_t = α_max（与 warmup 末端衔接）
- t = T_c: cos(π) = −1 -> α_t = α_min
- t = 0: warmup 从 0 开始线性上升（返回 0）
- 整数除法陷阱: t/T_w 要是浮点除
- t > T_c 之后恒为 α_min（不会回升）

测试:
- 五个关键点手算:
  t=0 -> 0；t=T_w -> α_max；t=(T_w+T_c)/2 -> (α_max+α_min)/2；
  t=T_c -> α_min；t=T_c+100 -> α_min
- 曲线连续（分段衔接处无跳变）？
- uv run pytest -k test_get_lr_cosine_schedule
```
