# 07 · 精确行级去重（exact_deduplication）

讲义 §3.1 ｜ 3 分 ｜ adapter: `run_exact_line_deduplication` ｜ 测试: `uv run pytest -k test_exact_line_deduplication`

```text
Problem:
exact_deduplication — 两遍扫描，删掉语料中出现超过一次的所有行

输入:
input_paths: list[str]    输入文件路径列表
output_dir: str           输出目录

输出:
每个输入文件重写到 output_dir 下"同名"文件，
只保留在整个语料中出现恰好一次的行
（a/1.txt, a/2.txt -> b/1.txt, b/2.txt）

参数:
无

公式:
保留条件: count(line) == 1（跨全部输入文件统计）
注意: 出现 2 次以上的行"全部删除"，不是保留一份！
（页头/菜单/页脚这类模板行会被整体清掉）

步骤:
1. 第一遍: 遍历所有文件所有行，
   key = hashlib.md5(line.encode()).hexdigest()（省内存，不存原文）
   counts[key] += 1
2. 第二遍: 逐文件重写——只写 counts[hash(line)] == 1 的行，
   保持原有行序
3. 输出文件名 = 输入文件名（os.path.basename），目录换成 output_dir

需要查的 API:
- hashlib.md5 / sha1（稳定 hash；内置 hash() 每进程随机化，不要用）
- collections.Counter / defaultdict(int)
- pathlib.Path.name / os.path.join；os.makedirs(exist_ok=True)

边界情况:
- 行的口径: 按 "\n" 分割；hash 前是否 strip 要前后两遍一致
- 空行也是行（大量空行会互为重复被删）
- 用 hash 做 key: 理论有碰撞但 md5 上可忽略；千万别用 Python hash()
- 大语料: 计数字典的内存 = 唯一行数 × 摘要大小（这正是用 hash 的原因）
- 输出目录不存在要先创建

测试:
- uv run pytest -k test_exact_line_deduplication
- 手算: 文件A=[x, y]，文件B=[x, z] -> A'=[y]，B'=[z]
- 幂等性: 对输出再跑一遍应无变化
```
