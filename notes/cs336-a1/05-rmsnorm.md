# 05 · RMSNorm（rmsnorm）

讲义 §3.4.1 ｜ 1 分 ｜ adapter: `adapters.run_rmsnorm` ｜ 测试: `uv run pytest -k test_rmsnorm`

```text
Problem:
RMSNorm — 均方根层归一化 (nn.Module)

输入:
x: (batch_size, sequence_length, d_model)   按 (..., d_model) 处理

输出:
(..., d_model)   与输入同 shape、同 dtype

参数:
g (gain): (d_model,)   初始化为全 1
超参: eps: float = 1e-5

公式:
RMS(a) = sqrt( (1/d_model) * Σ aᵢ² + eps )
RMSNorm(a)ᵢ = (aᵢ / RMS(a)) * gᵢ

步骤:
1. in_dtype = x.dtype；x = x.to(torch.float32)          # 防平方溢出
2. rms = torch.sqrt( x.pow(2).mean(dim=-1, keepdim=True) + eps )
3. result = x / rms * self.g                            # g 沿最后一维广播
4. return result.to(in_dtype)                           # 转回原 dtype

需要查的 API:
- Tensor.mean(dim=-1, keepdim=True)
- torch.sqrt / Tensor.pow
- Tensor.to(torch.float32) / Tensor.to(in_dtype)
- nn.Parameter(torch.ones(d_model))

边界情况:
- dtype: 必须先升 float32 再平方（fp16/bf16 平方会上溢），返回前转回原 dtype
- batch 维: 前面任意多个 batch 维，只对最后一维 d_model 归一化
- epsilon: 加在均值之后、sqrt 之内（不是加在除法分母外面）
- keepdim=True，否则 (..., ) 除 (..., d_model) 广播失败
- 分母是"均方根"不是"方差"——不减均值（区别于 LayerNorm）

测试:
- shape 是否与输入一样？
- dtype 是否与输入一样（内部升精度后要转回）？
- 手算: x=[3,4], g=1, 忽略eps -> mean(x²)=12.5, RMS≈3.5355 -> [0.8485, 1.1314]
- uv run pytest -k test_rmsnorm
```
