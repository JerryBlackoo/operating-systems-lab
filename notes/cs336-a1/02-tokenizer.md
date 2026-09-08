# 02 · BPE 分词器编解码（tokenizer）

讲义 §2.6 ｜ 15 分 ｜ adapter: `adapters.get_tokenizer` ｜ 测试: `uv run pytest tests/test_tokenizer.py`

```text
Problem:
Tokenizer 类 — 给定 vocab + merges，实现 encode / decode

输入:
__init__(vocab: dict[int, bytes],
         merges: list[tuple[bytes, bytes]],
         special_tokens: list[str] | None = None)
from_files(vocab_filepath, merges_filepath, special_tokens=None)  类方法
encode(text: str) -> list[int]
encode_iterable(iterable: Iterable[str]) -> Iterator[int]   惰性生成器（大文件）
decode(ids: list[int]) -> str

输出:
encode: token ID 列表；decode: 字符串

参数:
无可学习参数；内部需要反向词表 bytes -> id

公式:
encode = 预分词 -> 每个 pre-token 转 UTF-8 字节序列
         -> 按 merges 的"创建顺序"逐条应用合并 -> 查反向词表得 ID
decode = b"".join(vocab[id] for id in ids).decode("utf-8", errors="replace")

步骤:
1. 构造 bytes->id 反向词表；special_tokens 不在词表中则追加到末尾
2. encode: 先按 special tokens 切分文本，special token 直接映射为单个 ID
   （多个 special token 重叠时优先匹配最长的，按长度降序排入正则）
3. 普通片段: re.finditer(PAT) 预分词，pre-token -> [b, b, ...] 单字节列表
4. 反复扫描: 在当前序列里找到"merges 中最早创建"的可应用 pair，
   合并之；直到没有任何可应用 merge
5. encode_iterable: 逐块读入、逐块 encode、yield id（内存 O(1)，
   token 不得跨块边界）
6. decode: 拼接所有字节再统一 decode（不要逐 id decode！）

需要查的 API:
- regex: re.finditer / re.split / re.escape（捕获组切分可保留分隔符）
- bytes.decode("utf-8", errors="replace") -> 坏字节替换为 U+FFFD
- str.encode("utf-8")
- @classmethod；生成器 yield / yield from
- json 或 pickle（from_files 反序列化，格式与你 train_bpe 的存盘格式一致）

边界情况:
- 重叠 special token: "<|endoftext|><|endoftext|>" 要整体优先匹配（最长优先）
- 非法字节序列 decode 不能抛异常，用 errors="replace"（U+FFFD）
- 逐 id 单独 decode 会把跨 id 的多字节 UTF-8 字符切坏 —— 必须先拼接后 decode
- 空字符串 encode -> []；special_tokens=None 也要能跑
- encode_iterable: 不能把整个文件读进内存

测试:
- uv run pytest tests/test_tokenizer.py（与 tiktoken GPT-2 参考实现对拍）
- 往返一致: decode(encode(text)) == text（合法文本）
- 手算对照讲义例子: 'the cat ate' + 给定 vocab/merges -> [9, 7, 1, 5, 10, 3]
- encode_iterable 结果与一次性 encode 是否完全一致？内存是否恒定？
```
