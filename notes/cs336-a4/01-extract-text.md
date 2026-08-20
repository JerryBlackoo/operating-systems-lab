# 01 · HTML 转文本（extract_text）

讲义 §2.2 ｜ 3 分 ｜ adapter: `run_extract_text_from_html_bytes` ｜ 测试: `uv run pytest -k test_extract_text_from_html_bytes`

```text
Problem:
extract_text — 从原始 HTML 字节串中抽取纯文本

输入:
html_bytes: bytes    原始网页内容（编码不保证是 UTF-8！）

输出:
str    抽取出的纯文本

参数:
无

公式:
流程: bytes --(检测编码+解码)--> str --(HTML 抽取)--> 纯文本

步骤:
1. 先按 UTF-8 尝试 decode
2. 失败则用 resiliparse.parse.encoding.detect_encoding(html_bytes)
   检测编码，再按检测结果 decode（可加 errors="replace" 兜底）
3. resiliparse.extract.html2text.extract_plain_text(html_str) 抽取正文
4. 返回文本

需要查的 API:
- resiliparse.extract.html2text.extract_plain_text
- resiliparse.parse.encoding.detect_encoding
- bytes.decode(encoding, errors=)
- fastwarc.warc.ArchiveIterator / WarcRecordType（遍历 WARC 记录，
  record.reader.read() 取原始字节）

边界情况:
- 编码: ~98% 网页是 UTF-8，剩下的 GBK/latin-1 等必须能兜住（try/except）
- decode 失败别抛异常——detect 再试 + errors="replace"
- 抽取结果 ≠ 页面主内容: 菜单/页脚/表单文字都会混进来（后面 filter 处理）
- 空 HTML / 纯二进制内容输入
- WARC 里的 record 有多种类型，只处理 response 类型

测试:
- uv run pytest -k test_extract_text_from_html_bytes
- 自检: 在一个真实 WARC 上跑，与对应 WET 文件的抽取结果对比谁更干净
- 非 UTF-8 编码样例（如 GBK 中文页）能正确解码？
```
