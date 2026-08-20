# 03 · 个人身份信息遮蔽（mask_pii）

讲义 §2.4 ｜ 3 分 ｜ adapters: `run_mask_emails` / `run_mask_phone_numbers` / `run_mask_ips` ｜ 测试: `uv run pytest -k test_mask_emails` / `-k test_mask_phones` / `-k test_mask_ips`

```text
Problem:
mask_pii — 三个函数：遮蔽 email / 电话号码 / IPv4 地址

输入:
text: str

输出:
(new_text: str, count: int)    替换后的文本 + 替换次数
替换串分别为:
  "|||EMAIL_ADDRESS|||"  "|||PHONE_NUMBER|||"  "|||IP_ADDRESS|||"

参数:
无（纯正则）

公式:
email 正则（够用即可）:
  [A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}
US 电话（覆盖常见写法，容忍少量语法变化）:
  (?:\+?1[\s.-]?)?\(?\d{3}\)?[\s.-]?\d{3}[\s.-]?\d{4}
IPv4（每段 0-255）:
  \b(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}
     (?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\b

步骤:
1. 每类各写一个 re.compile 的正则
2. new_text, n = pattern.subn(replacement, text)
3. 返回 (new_text, n)

需要查的 API:
- re.subn（一次拿到替换文本和次数）
- re.compile；非捕获组 (?:...)；边界断言 \b
- 在线 regex 测试器（组合用例调试）

边界情况:
- 电话格式极多: (650) 555-1234 / 650-555-1234 / 650.555.1234 /
  6505551234 / +1 650 555 1234 —— 至少覆盖这些
- 别把普通长数字（订单号、年份串）误判成电话 —— 加边界/长度限制
- IPv4 每段 ≤255: 999.999.999.999 不应匹配；版本号 1.2.3.4 会被误伤
  （已知 false positive，写实验分析时提）
- email 里的点/加号/下划线；结尾 TLD ≥2 字母
- 计数用 subn 的返回值，别自己数

测试:
- uv run pytest -k test_mask_emails / test_mask_phones / test_mask_ips
- 手算: "call 650-555-1234 or (650) 555 1234" -> 2 次替换
- 替换后的文本不应残留任何原始 email/phone/IP 片段
```
