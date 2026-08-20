# 06 · 质量分类器（quality_classifier）

讲义 §2.7 ｜ 15 分 ｜ adapter: `run_classify_quality` ｜ 测试: `uv run pytest -k test_classify_quality`

```text
Problem:
quality_classifier — 训练一个 fastText 质量分类器并封装成打分函数

输入:
(a) 训练阶段:
    正例来源: Wikipedia 外链 URL 列表
      /shared-data/wiki/enwiki-20260501-extracted_urls.txt.gz（需子采样）
    负例来源: 随机 Common Crawl 页面
(b) 推理阶段: text: str

输出:
(a) 训练好的 fastText 模型文件
(b) (label, score)：高质量与否 + 置信度

参数:
fastText 监督分类器（自己训练，不是预训练）

公式:
思路（WebText/LLaMA 做法）: 被 Wikipedia 引用的页面 ≈ 高质量正例
训练数据格式（fastText 监督格式，一行一条）:
  __label__high <正例文本(单行)>
  __label__low  <负例文本(单行)>

步骤:
1. 子采样正例 URL；用 wget 批量抓取:
   wget --timeout=5 -i urls.txt --warc-file=positive -O /dev/null
2. 用 01 的 extract_text 抽正文；正例也要先过语言识别/Gopher 规则清洗
   （wiki 外链也有死链/垃圾页）
3. 从 CC 随机抽同量级负例，同样抽正文
4. 生成训练文件: 去换行（替换为空格），打上 __label__high/low，
   划出验证集
5. model = fasttext.train_supervised(input="train.txt",
       lr=0.1, epoch=?, wordNgrams=2)；用验证集调参
6. 推理函数: predict -> 去前缀 -> (label, score)

需要查的 API:
- fasttext.train_supervised(input, lr, epoch, wordNgrams, dim)
- model.test("valid.txt")（P/R 评估）；model.save_model / load_model
- wget -i / --warc-file / --timeout
- gzip.open（读 URL 列表）；random.sample（子采样）

边界情况:
- fastText 每条训练样本必须是"单行"——文本里的 \n 全部替换掉
- 正负例数量要平衡；总量几千~几万条即可起步
- 正例不全干净: 先过滤再当正例，否则分类器学到噪声
- 抓取要限时（--timeout=5），大量 URL 会死链/超时，属正常
- 标签名与 adapter 的期望输出对齐
- 阈值权衡: 下游过滤时的分数线决定 precision/recall

测试:
- uv run pytest -k test_classify_quality（两个 sanity 样例）
- 自检: 一段 wiki 风格正文 -> high 高分；导航/垃圾页文本 -> low
- model.test 的验证集 P@1 是否明显高于 0.5？
```
