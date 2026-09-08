# 01 · IsoFLOPs 缩放定律拟合（chinchilla_isoflops）

讲义 §2.1 ｜ 5 分 ｜ 无 pytest（交付物是脚本 + 两张外推图）

```text
Problem:
chinchilla_isoflops — 用给定的训练数据复现 Chinchilla IsoFLOPs 方法

输入:
data/isoflops_curves.json — JSON 数组，每个元素一次训练:
  { "parameters": N, "compute_budget": C, "final_loss": L }

输出:
- 图 1: N_opt vs C 幂律（数据点 + 拟合线，外推到 ≥1e24 FLOPs）
- 图 2: D_opt vs C 幂律（同上）
- 预测: C=1e23 和 C=1e24 时的最优模型大小 N_opt 与数据量 D_opt

参数:
无（离线曲线拟合）

公式:
训练 FLOPs 近似:  C = 6·N·D    =>    D = C / (6N)
每个固定预算 C_i 的一组 runs = 一条 "IsoFLOPs profile"（loss-N 呈 U 形）
N_opt(C_i) = 该组中 final_loss 最小的 run 的参数量（讲义建议直接取最小，
             不必像原论文那样先拟合二次函数）
D_opt(C_i) = C_i / (6·N_opt(C_i))
幂律: N_opt = k·C^a，D_opt = k'·C^b   （log-log 空间是直线）

步骤:
1. json.load 读入，按 compute_budget 分组
2. 每组取 final_loss 最小的 run -> (C_i, N_opt(C_i))
3. D_opt(C_i) = C_i / (6·N_opt)
4. 在 log-log 空间线性拟合: np.polyfit(log(C), log(N_opt), 1) 得 a 和 log k
   （或 scipy.optimize.curve_fit 直接拟幂函数）
5. 外推 C=1e23、1e24；matplotlib loglog 画数据点 + 拟合线 + 外推段

需要查的 API:
- json.load
- np.polyfit / np.log / np.exp（log 空间线性回归）
- scipy.optimize.curve_fit（若直接拟合幂函数）
- matplotlib: plt.loglog / plt.scatter

边界情况:
- 一定在 log 空间拟合：幂律在线性空间直接 curve_fit 容易被大数值主导
- sanity: a + b ≈ 1（因为 N_opt·D_opt ∝ C，Chinchilla 报 a≈0.49, b≈0.51）
- 外推跨多个数量级：拟合点的少量噪声在外推端被放大，图上画出外推区间
- 每组直接取 min 而非二次拟合：数据点稀疏时二次拟合反而不稳
- 单位: FLOPs 用浮点存储（6e18 等），小心整数溢出习惯

测试:
- 无 pytest；自检:
- 每条 IsoFLOPs profile 的 loss-N 曲线是否呈 U 形（画出来看）？
- 拟合指数 a、b 是否落在 0.4~0.6 的合理区间？a+b≈1？
- 1e23/1e24 的预测值代回 C=6ND 是否自洽？
```
