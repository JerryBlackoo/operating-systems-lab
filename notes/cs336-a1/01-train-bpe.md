# 01 · BPE 分词器训练（train_bpe）

讲义 §2.4–2.5 ｜ 15 分 ｜ adapter: `adapters.run_train_bpe` ｜ 测试: `uv run pytest tests/test_train_bpe.py`

```text
Problem:
train_bpe — 训练字节级 BPE 分词器（纯函数，不是 nn.Module）

输入:
input_path: str            训练语料文本文件路径
vocab_size: int            最终词表大小上限（含 256 个初始字节 + 合并产物 + special tokens）
special_tokens: list[str]  加入词表的特殊 token，如 "<|endoftext|>"

输出:
vocab: dict[int, bytes]               token ID -> token 字节串
merges: list[tuple[bytes, bytes]]     合并列表，按"创建顺序"排列

参数:
无可学习参数（离线训练算法）
合并次数 = vocab_size - 256 - len(special_tokens)

公式:
初始词表 = special_tokens + 全部 256 个字节值
每轮迭代:
  pair_counts = 统计所有 pre-token 内部相邻字节对的加权频次
  best = 频次最高的 pair；平局时取字典序更大者: max(tied_pairs)
  合并 best -> 新 token（bytes 拼接），加入词表，记入 merges

步骤:
1. 读文件；用 re.split("|".join(map(re.escape, special_tokens))) 按 special token
   切成段（special token 是硬边界，本身不参与统计）
2. 对每段用 GPT-2 正则 PAT 做 re.finditer 预分词，统计频次表
   dict[tuple[bytes, ...], int]（每个 pre-token 是单字节 bytes 的元组）
3. 由频次表构建 pair 计数索引（pair -> 总频次，pair -> 出现在哪些 pre-token）
4. 循环 num_merges 次: 取最高频 pair（平局取 max）-> 记录 merge ->
   只增量更新与被合并 pair 重叠的计数（不要全量重扫！）
5. 组装 vocab: special tokens + 256 字节 + 按顺序的合并产物；返回 (vocab, merges)

需要查的 API:
- regex 包（不是内置 re）: re.finditer / re.split / re.escape
- GPT-2 PAT: r"""'(?:[sdmt]|ll|ve|re)| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+"""
- collections.Counter / defaultdict
- multiprocessing.Pool（并行预分词）+ 官方 chunk 边界代码 pretokenization_example.py
- bytes 拼接 b"a"+b"b"；tuple[bytes,bytes] 的 max() 比较规则

边界情况:
- 平局规则: 取字典序更大的 pair，如 max 会选 ("BA","A") 而不是 ("A","C")
- 合并不能跨 pre-token 边界，也不能跨 special token 边界
- special token 不参与 merge 统计，但占用 vocab_size 名额
- 单个字节也是 bytes 对象（Python 没有单字节标量类型）
- 大语料: 预分词是瓶颈，用 multiprocessing 分 chunk（边界对齐到 <|endoftext|>）

测试:
- uv run pytest tests/test_train_bpe.py（含 test_train_bpe_special_tokens 专测边界切分）
- merges 顺序是否严格按创建顺序？
- 手算对照讲义例子: 语料 low*5 lower*2 widest*3 newest*6
  -> 第一次合并 ('s','t')（与 ('e','s') 打平后取字典序大者），
  前 6 次 merges = [s t, e st, o w, l ow, w est, n e]
- vocab 大小 = 256 + len(special_tokens) + len(merges) 是否成立？
```
