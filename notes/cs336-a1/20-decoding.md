# 20 · 文本生成 / 解码（decoding）

讲义 §6 ｜ 3 分 ｜ 无 pytest（交付物是解码函数 + 生成样例）

```text
Problem:
decode — 自回归采样生成文本，支持温度缩放与 top-p（nucleus）采样

输入:
model: TransformerLM
prompt: 已编码的 token ID 序列 x_{1..t}
max_new_tokens: int      生成上限
temperature: τ           softmax 温度
top_p: p                 nucleus 阈值
eos_token_id: <|endoftext|> 的 ID（遇到即停）

输出:
生成的 token ID 序列（解码成文本给人看）

参数:
无（推理期函数）

公式:
v = TransformerLM(x_{1..t})[最后一个位置]  ∈ R^vocab_size
温度: softmax(v, τ)ᵢ = exp(vᵢ/τ) / Σⱼ exp(vⱼ/τ)     τ→0 趋近 argmax
top-p: 取最小集合 V(p) 使 Σ_{j∈V(p)} qⱼ ≥ p（q 按降序累加），
       集合外概率置 0，集合内重新归一化后采样

步骤:
1. model.eval()；torch.no_grad() 下循环 max_new_tokens 次:
2. 输入超过 context_length 时截断（只保留最近 ctx 个 token）
3. logits = model(x)[..., -1, :]      # 只要最后一个位置
4. probs = softmax(logits / τ)
5. top-p 过滤: sort 降序 -> cumsum -> 保留累计 ≥ p 的最小前缀
   （保证至少留 1 个）-> 其余置 0 -> renormalize
6. next = torch.multinomial(probs, num_samples=1)；拼到 x 末尾
7. next == eos_token_id 则 break；最后 tokenizer.decode 输出

需要查的 API:
- torch.multinomial（按概率采样）
- torch.sort(descending=True) / torch.cumsum
- torch.no_grad() / model.eval()
- Tensor.scatter_ 或按排序索引回填 mask
- tokenizer.decode

边界情况:
- τ 很小/为 0: 除法爆炸 -> 特判为 argmax（或 clamp τ）
- top-p 边界: 即使第一个 token 概率已 ≥ p 也必须保留它（至少一个）
- 累加判断用"≥ p 的最小前缀"，边界 token 要包含在内
- 序列长度不能超过 context_length（RoPE buffer 上限），要滑窗截断
- 别忘了停止条件: eos 或 max_new_tokens
- 每步只取最后一个位置的 logits（前面位置是给训练用的）

测试:
- 无 pytest；自检手段:
- τ→0 时是否退化为贪心（可重复生成相同文本）？
- p=1.0 时是否等价于不做 top-p？
- 生成遇 <|endoftext|> 是否停止？
- TinyStories 模型: 生成 ≥256 token，肉眼检查是否像英文小故事（讲义有参考样例）
```
