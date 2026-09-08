# 02 · 提示词与回复的分词（tokenize_prompt_and_output）

讲义 §4.2.1 ｜ 1 分 ｜ adapter: `run_tokenize_prompt_and_output` ｜ 测试: `uv run pytest -k test_tokenize_prompt_and_output`

```text
Problem:
tokenize_prompt_and_output — 分别分词 prompt/response，拼接并构造 response_mask

输入:
prompt_strs: list[str]
output_strs: list[str]
tokenizer: PreTrainedTokenizer

输出:
dict，设 L = max(每条 prompt+output 拼接后的 token 长度):
  input_ids:     (batch_size, L-1)   拼接序列去掉"最后"一个 token
  labels:        (batch_size, L-1)   拼接序列去掉"第一"个 token（右移一位）
  response_mask: (batch_size, L-1)   与 labels 对齐；label 来自 response 则 1，
                                     prompt/padding 位置为 0

参数:
无

公式:
seq = prompt_ids + response_ids     # 直接拼接：无 BOS/EOS/分隔符！
input_ids = seq[:-1]；labels = seq[1:]
mask[t] = 1  <=>  labels[t] 是 response 的 token

步骤:
1. 每条分别: tokenizer(prompt, add_special_tokens=False)["input_ids"]，
   response 同理
2. 拼接 seq；记录 prompt_len 与总长
3. pad 到 batch 内最长（pad_token_id；缺省可用 eos）
4. input_ids = padded[:, :-1]；labels = padded[:, 1:]
5. mask: 位置 t 的 label 对应原序列位置 t+1，
   属于 [prompt_len, prompt_len+response_len) 区间则 1，否则 0

需要查的 API:
- tokenizer.__call__(add_special_tokens=False)
- tokenizer.pad_token_id / eos_token_id
- torch.full / torch.zeros + 切片赋值（手动 padding 更可控）

边界情况:
- add_special_tokens=False：绝不能让 tokenizer 偷偷加 BOS/EOS
- mask 与 labels 对齐，不是与 input_ids 对齐（错一位是最常见 bug）
- response 的第一个 token: 它作为 label 出现在位置 prompt_len-1，
  该位置 mask=1
- padding 位置 mask=0（loss 不算）
- batch 内长度不一，注意 L 是"拼接后最长"

测试:
- uv run pytest -k test_tokenize_prompt_and_output
- 手算: prompt=[1,2], response=[3,4] -> seq=[1,2,3,4]
  input_ids=[1,2,3]，labels=[2,3,4]，mask=[0,1,1]
- mask.sum() 是否等于各 response 的 token 数之和？
```
