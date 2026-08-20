# 05 · Gopher 质量规则（gopher_quality_filters）

讲义 §2.6 ｜ 3 分 ｜ adapter: `run_gopher_quality_filter` ｜ 测试: `uv run pytest -k test_gopher`

```text
Problem:
gopher_quality_filters — 基于启发式规则的低质量文档过滤

输入:
text: str

输出:
bool    True = 通过全部质量规则（保留），False = 任一规则不过（丢弃）

参数:
无（纯规则，出自 Gopher 论文 Appendix A 的子集）

公式:
必须全部满足:
1. 词数 ∈ [50, 100000]
2. 平均词长 ∈ [3, 10] 字符
3. 以省略号 "..." 结尾的行占比 ≤ 30%
4. 含至少一个字母的词占比 ≥ 80%

步骤:
1. words = nltk.word_tokenize(text)（或自定分词）
2. 规则1: 50 <= len(words) <= 100_000
3. 规则2: 3 <= sum(len(w) for w in words)/len(words) <= 10
4. 规则3: lines = text.split("\n")；
   sum(l.rstrip().endswith("...") for l in lines)/len(lines) <= 0.30
5. 规则4: sum(any(c.isalpha() for c in w) for w in words)/len(words) >= 0.80
6. 全过返回 True

需要查的 API:
- nltk.word_tokenize（首次需 nltk.download("punkt")/"punkt_tab"）
- str.isalpha / any
- str.split("\n") / str.endswith

边界情况:
- 空文档 / 全空白: 直接 False（避免除以 0！）
- 词的口径要一致: 平均词长与字母占比都基于同一份分词结果
- 省略号是三个点 "..."（注意 Unicode "…" 是否也算——按测试为准，
  稳妥做法两种都算）
- 行数为 0（无换行文本 = 1 行）
- 规则是"与"关系: 一条不过全盘 False

测试:
- uv run pytest -k test_gopher
- 手算: 49 个词 -> False；一段正常英文文章 -> True
- 构造边界样例: 恰 50 词、平均词长恰 3、恰 30% 省略号行
```
