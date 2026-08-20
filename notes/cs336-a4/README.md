# CS336 Assignment 4 (data) — 编程题速学卡片

来源: CS336 Spring 2026, Assignment 4: Filtering Language Modeling Data (v26.0.1)。
配套仓库: [stanford-cs336/assignment4-data](https://github.com/stanford-cs336/assignment4-data)。

## 全局背景

- 主线：把 Common Crawl 原始网页变成语言模型训练数据 —— 抽取 → 过滤 → 去重 → tokenize → 训练验证。
- 最终目标：训练 GPT-2 small（约 430M 参数）使 Paloma 的 C4-100-domains 验证困惑度最低；**只许改数据，不许改模型和训练脚本**。
- 工具链：`fastwarc`（读 WARC/WET）、`resiliparse`（HTML 抽取/编码检测）、`fasttext`（语言/有害/质量分类）、`nltk`（分词）、`mmh3`（minhash）。
- 红线：Paloma 验证集可以用来"设计"过滤器，但绝不能把验证文本拷进训练集。

## 编程题卡片索引（10 张）

| # | 卡片 | Problem | 分值 | 讲义 | 测试 |
|---|------|---------|------|------|------|
| 01 | [extract_text](01-extract-text.md) | HTML 字节转纯文本 | 3 | §2.2 | `-k test_extract_text_from_html_bytes` |
| 02 | [language_identification](02-language-identification.md) | fastText 语言识别 | 6 | §2.3 | `-k test_identify_language` |
| 03 | [mask_pii](03-mask-pii.md) | 遮蔽 email/电话/IP | 3 | §2.4 | `-k test_mask_emails` / `-k test_mask_phones` / `-k test_mask_ips` |
| 04 | [harmful_content](04-harmful-content.md) | NSFW/毒性检测 | 6 | §2.5 | `-k test_classify_nsfw` / `-k test_classify_toxic_speech` |
| 05 | [gopher_quality_filters](05-gopher-quality-filters.md) | Gopher 质量规则 | 3 | §2.6 | `-k test_gopher` |
| 06 | [quality_classifier](06-quality-classifier.md) | 训练 fastText 质量分类器 | 15 | §2.7 | `-k test_classify_quality` |
| 07 | [exact_deduplication](07-exact-deduplication.md) | 精确行级去重 | 3 | §3.1 | `-k test_exact_line_deduplication` |
| 08 | [minhash_deduplication](08-minhash-deduplication.md) | MinHash+LSH 模糊去重 | 8 | §3.2 | `-k test_minhash_deduplication` |
| 09 | [filter_data](09-filter-data.md) | 并行过滤主管线 | 6 | §4 | 无 pytest（统计报告） |
| 10 | [tokenize_data](10-tokenize-data.md) | GPT-2 tokenize + uint16 序列化 | 2 | §4 | 无 pytest（token 数报告） |

依赖链：01 是一切的入口；02~06 是单文档过滤原语；07~08 是语料级去重；09 把 01~08 组装成管线；10 产出训练脚本要的 .bin。

## 非编程题清单（探索 / 实验）

| Problem | 类型 | 分值 | 内容 |
|---------|------|------|------|
| look_at_cc | 探索 | 4 | 浏览 WARC/WET 原始数据，标注 25 个文档 |
| （各编程题的子问） | 实验 | — | 02(b,c)/03(d,e)/04(c,d)/05(b): 在真实 WARC 上跑并人工对比 20 条、定阈值 |
| inspect_filtered_data | 检查 | 4 | 抽查保留的 5 条 + 丢弃的 5 条，判断过滤是否合理并迭代 |
| train_model | 实验 | 8 | 用提供的 train.py（8×B200, batch 128/卡, 16384 步）训练 430M 模型，报告最佳验证 loss 与曲线 |
