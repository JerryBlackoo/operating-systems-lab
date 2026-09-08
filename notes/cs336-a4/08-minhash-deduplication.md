# 08 · MinHash + LSH 模糊去重（minhash_deduplication）

讲义 §3.2 ｜ 8 分 ｜ adapter: `run_minhash_deduplication` ｜ 测试: `uv run pytest -k test_minhash_deduplication`

```text
Problem:
minhash_deduplication — 文档级模糊去重：
minhash 签名 -> LSH 分桶找候选 -> 真实 Jaccard 验证 -> 聚簇随机保留一个

输入:
input_paths: list[str]     每个文件一个文档
num_hashes: k              minhash 签名长度（k 个不同 seed 的 hash 函数）
num_bands: b               LSH band 数（保证 b 整除 k，r = k/b）
ngram_length: n            词级 n-gram 长度
jaccard_threshold: t       判定重复的相似度阈值
output_dir: str            输出目录

输出:
重写文件到 output_dir 同名；每个重复簇随机保留 1 个文档，其余不写出

参数:
无

公式:
Jaccard(S, T) = |S ∩ T| / |S ∪ T|
minhash(h_i, S) = min_{s∈S} h_i(s)
签名 = [minhash(h_1,S), ..., minhash(h_k,S)]
性质: P(两签名某列相同) = Jaccard 相似度（签名列相同比例 ≈ Jaccard）
LSH: 签名切成 b 个 band（每 band r 个值），任一 band 完全相同 -> 候选对
b 越大 -> 召回越高、精确率越低

步骤:
1. 归一化文本（签名前必须做！提升召回）:
   小写、去标点、空白归一为单空格、NFD 分解去重音
2. 每文档: 分词 -> 词级 n-gram 集合
3. k 个 hash（如 mmh3.hash(ngram, seed=i)），逐 n-gram 取 min，
   得长度 k 的签名
4. 签名按 band 切分: band_key = (band_idx, tuple(band 内 r 个值))；
   同 key 的文档进同一桶，桶内两两为候选对
5. 候选对算"真实" n-gram Jaccard（同样在归一化文本上），
   ≥ t 判为重复
6. 并查集聚簇（A~B、B~C => A,B,C 一簇）
7. 每簇随机保留 1 个文档；重写保留文档到 output_dir 同名文件

需要查的 API:
- mmh3.hash(s, seed) 或 hashlib（多 seed 模拟多 hash 函数）
- unicodedata.normalize("NFD", s) + unicodedata.combining 滤重音
- string.punctuation / re.sub（去标点、归一空白）
- 并查集: 自写 find/union（路径压缩）
- itertools.combinations（桶内配对）；random.seed / choice

边界情况:
- 归一化要在"签名"和"真实 Jaccard"两处一致使用
- LSH 有假阳性 -> 候选对必须用真实 Jaccard 复核；
  也有假阴性（band 全不同）-> 属于方法固有，接受
- 文档 n-gram 数 < n（太短）: 特判（空集合的 Jaccard）
- 簇的传递合并必须做（不能只删桶内 pair）
- 随机保留要固定 seed 才可复现
- k 必须被 b 整除（题目保证，代码里 assert 一下）

测试:
- uv run pytest -k test_minhash_deduplication
- 手算: 签名 [1,2,3,4,5,6] vs [1,2,3,5,1,2]，b=3 r=2:
  band1 [1,2]==[1,2] -> 候选；其他 band 不同（不影响候选身份）
- 两份只差年份/人名的 MIT License 应被判重；无关文档不应
```
