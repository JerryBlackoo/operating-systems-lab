# 03 · 回复的逐 token 对数概率（get_response_log_probs）

讲义 §4.2.1 ｜ 1 分 ｜ adapter: `run_get_response_log_probs` ｜ 测试: `uv run pytest -k test_get_response_log_probs`

```text
Problem:
get_response_log_probs — 从因果 LM 取每个 label token 的条件对数概率
（可选：每个位置的下一 token 分布熵）

输入:
model: PreTrainedModel（HF 模型，调用方负责放对设备/是否 no_grad）
input_ids: (batch_size, sequence_length)
labels:    (batch_size, sequence_length)
return_token_entropy: bool = False

输出:
dict:
  "log_probs":     (batch_size, sequence_length)  log p_θ(labels_t | x_<t)
  "token_entropy": 同 shape（仅当 return_token_entropy=True）

参数:
无新增参数（用现成模型打分）

公式:
logits = model(input_ids).logits            (B, S, V)
log_probs_full = log_softmax(logits, -1)
log_probs[b,t] = log_probs_full[b, t, labels[b,t]]
entropy[b,t] = −Σ_v p(v) log p(v)
             = logsumexp(logits) − Σ_v softmax(logits)·logits   （稳定式）

步骤:
1. logits = model(input_ids).logits
2. logp = F.log_softmax(logits, dim=-1)     # 内部已做减 max，数值稳定
3. log_probs = torch.gather(logp, -1, labels.unsqueeze(-1)).squeeze(-1)
4. 若要熵: probs = logp.exp()；entropy = −(probs * logp).sum(-1)
   （或 logsumexp 形式，避免物化 probs）
5. 返回 dict

需要查的 API:
- model(input_ids).logits（HF CausalLM 输出）
- torch.nn.functional.log_softmax
- torch.gather(dim=-1, index=)（index 要 unsqueeze 到同维数）
- torch.logsumexp

边界情况:
- 一定用 log_softmax 而不是 softmax().log()（下溢）
- bfloat16 模型: 建议把 logits 升 float32 再算（精度）
- gather 的 index 必须 int64 且形状 (B, S, 1)
- 熵只用于日志: 训练里对它 no_grad/detach，省显存
- (B,S,V) 的 V=词表 5 万+，中间张量很大——微批场景注意显存

测试:
- uv run pytest -k test_get_response_log_probs
- 手算: 2 分类 logits=[0,0] -> log_prob=log 0.5, entropy=log 2
- log_probs 全部 ≤ 0？entropy ≥ 0？
```
