# 04 · 有害内容识别（harmful_content）

讲义 §2.5 ｜ 6 分 ｜ adapters: `run_classify_nsfw` / `run_classify_toxic_speech` ｜ 测试: `uv run pytest -k test_classify_nsfw` / `-k test_classify_toxic_speech`

```text
Problem:
harmful_content — 两个函数：NSFW 检测 + 毒性言论检测

输入:
text: str

输出:
(label: str, score: float)
  NSFW:  "nsfw" / "non-nsfw" + 置信度
  毒性:  "toxic" / "non-toxic" + 置信度

参数:
Dolma 项目提供的 Jigsaw fastText 预训练分类器:
- /shared-data/classifiers/dolma_fasttext_nsfw_jigsaw_model.bin
- /shared-data/classifiers/dolma_fasttext_hatespeech_jigsaw_model.bin

公式:
与语言识别同构: model.predict -> ("__label__xxx", prob)
label = 去前缀后的标签；score = 概率

步骤:
1. 模块级加载两个模型（各一次）
2. 预处理: 替换 "\n" 为空格
3. labels, probs = model.predict(cleaned_text)
4. 去掉 "__label__" 前缀，按需映射到测试期望的标签名
5. 返回 (label, float(probs[0]))

需要查的 API:
- fasttext.load_model / predict
- 模型输出的原始标签名（先 predict 一条看看是什么再写映射）

边界情况:
- 标签名要与 adapter/测试预期一致（先打印确认: __label__nsfw 等）
- 换行必须先替换（fastText 限制）
- 提供的测试只是 Jigsaw 上的 sanity check，不代表分类器准 ——
  实验题要求人工标 20 条对比、给出错误分析和阈值建议
- 分类器训练自 Wikipedia 评论，对网页正文有分布偏移
- 过滤阈值权衡: 太严损失多样性，太松留下有害内容

测试:
- uv run pytest -k test_classify_nsfw
- uv run pytest -k test_classify_toxic_speech
- 自检: 干净文本 -> non-* 高分；明显脏话样例 -> 对应标签
```
