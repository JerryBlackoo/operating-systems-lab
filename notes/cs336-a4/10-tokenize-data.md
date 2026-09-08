# 10 · 数据 tokenize 与序列化（tokenize_data）

讲义 §4 ｜ 2 分 ｜ 无 pytest（交付物是脚本 + token 总数）

```text
Problem:
tokenize_data — 用 GPT-2 tokenizer 把过滤后的文本编码成 uint16 二进制

输入:
过滤后的文本数据（每文档一行/一段）

输出:
一个 .bin 文件: np.uint16 的 token ID 数组，ids_array.tofile(output_path)
（与提供的训练脚本 scripts/train.py --train-bin 兼容）

参数:
无（tokenizer 用现成的 GPT-2，不自己训练）

公式:
每个文档: ids = tokenizer.encode(doc) + [tokenizer.eos_token_id]
全部拼接成一维数组 -> np.array(dtype=np.uint16) -> tofile

步骤:
1. tokenizer = AutoTokenizer.from_pretrained("gpt2")
2. 定义 tokenize_line_and_add_eos(line):
   return tokenizer.encode(line) + [tokenizer.eos_token_id]
3. multiprocessing.Pool(cpu_count()) + pool.imap(fn, lines, chunksize=100)
   并行编码（tqdm 看进度）
4. 展平所有 id -> np.array(all_ids, dtype=np.uint16)
5. ids_array.tofile(output_path)；打印 token 总数（交付要求）

需要查的 API:
- transformers.AutoTokenizer.from_pretrained("gpt2")
- tokenizer.encode / tokenizer.eos_token_id（<|endoftext|> = 50256）
- multiprocessing.Pool.imap(chunksize=)
- np.ndarray.tofile / np.fromfile(dtype=np.uint16)（读回验证）

边界情况:
- 每个文档后必须接 <|endoftext|>（文档边界，训练脚本依赖）
- dtype 必须 uint16: GPT-2 词表 50257 < 65536，刚好放得下
- tofile 是裸二进制无 header——读回必须显式指定 dtype
- 超长行 encode 可能触发 tokenizer 的 max length 警告（仅警告，
  该拼接场景可忽略/分块）
- 内存: 先分片写多个文件再 cat，或流式追加，避免一次性攒几十 GB

测试:
- 无 pytest；自检:
- np.fromfile(out, dtype=np.uint16)[:2000] 用 tokenizer.decode 还原，
  肉眼检查文本 + 文档间有 <|endoftext|>
- token 总数是否与文本量匹配（英文 ~4 字节/token 粗估）
- max(ids) < 50257？
```
