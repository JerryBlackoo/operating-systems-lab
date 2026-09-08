# 02 · 语言识别（language_identification）

讲义 §2.3 ｜ 6 分 ｜ adapter: `run_identify_language` ｜ 测试: `uv run pytest -k test_identify_language`

```text
Problem:
language_identification — 判断文本的主要语言 + 置信度

输入:
text: str    Unicode 字符串

输出:
(lang_id: str, score: float)    如 ("en", 0.94)；score ∈ [0, 1]

参数:
预训练模型: fastText lid.176.bin
（/shared-data/classifiers/lid.176.bin，支持 176 种语言）

公式:
fastText 输出: (("__label__en",), (0.94,))
lang_id = 标签去掉 "__label__" 前缀；score 取对应概率

步骤:
1. 模块级加载一次: model = fasttext.load_model("lid.176.bin")
   （别在函数里反复加载，非常慢）
2. 预处理: text.replace("\n", " ")   # fastText predict 不接受换行
3. labels, probs = model.predict(cleaned_text)
4. lang = labels[0].removeprefix("__label__")；score = float(probs[0])
5. score = min(max(score, 0.0), 1.0)   # 概率偶尔浮点溢出 >1
6. 必要时做标签映射（测试认 "en" 和 "zh"）

需要查的 API:
- fasttext.load_model / model.predict(text, k=1)
- str.removeprefix / replace
- （用于实验）随机抽样 WARC 文档人工对比

边界情况:
- predict 传入含 "\n" 的字符串会直接报错——必须先替换
- 标签映射: lid.176 输出 "zh" 等简写，与测试预期一致即可
- 空字符串 / 极短文本的预测很不可靠
- score 可能是 1.0000001 之类，clamp 到 [0,1]
- 下游过滤时要选置信度阈值（实验题：人工标 20 条对比后定）

测试:
- uv run pytest -k test_identify_language（英文 + 中文两个用例）
- 自检: "Hello world" -> ("en", 高分)；中文段落 -> ("zh", 高分)
- 混合语言文本给出的是"主"语言？
```
