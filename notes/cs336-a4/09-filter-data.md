# 09 · 数据过滤主管线（filter_data）

讲义 §4 ｜ 6 分 ｜ 无 pytest（交付物是并行过滤脚本 + 各过滤器统计）

```text
Problem:
filter_data — 把全部过滤原语串成管线，处理 2500 个英文 WET 文件，
产出使 C4-100-domains 验证困惑度最低的训练数据

输入:
/shared-data/english-wet-data 下的 WET 文件
（已按 fastText lid 英文概率 ≥70% 预筛过）

输出:
过滤后的文本数据 + 每个过滤步骤的保留/丢弃统计

参数:
各过滤器阈值（语言分、质量分、有害分、Gopher 规则开关、去重参数）
—— 这些是你要调的"数据超参"

公式:
目标: min 验证困惑度(C4 100 domains / Paloma)
约束: 不许改模型和训练脚本；不许把验证数据拷进训练集
     （但"允许"用验证数据来设计过滤器/分类器，如训练风格分类器）

步骤:
1. 单文件处理函数 process_single_wet_file(in_path, out_path):
   fastwarc ArchiveIterator 遍历 record -> 取文本 ->
   依次过滤（便宜的在前）:
   a. Gopher 规则（纯规则，最便宜）
   b. 语言识别分数 ≥ 阈值
   c. 有害内容（NSFW/toxic）分数过滤
   d. 质量分类器分数 ≥ 阈值
   e. PII 遮蔽（不丢文档，只改文本）
   每步维护 counter: 该步丢了多少文档
2. ProcessPoolExecutor(max_workers=len(os.sched_getaffinity(0)))
   并行处理所有 WET 文件；tqdm + as_completed 跟进度
3. 汇总各步统计（交付要求: 每个过滤器的丢弃占比）
4. 最后跑去重: 精确行去重 +（可选）MinHash 文档去重
5. 记录总耗时，外推整个 Common Crawl 的处理时间（书面题）

需要查的 API:
- concurrent.futures.ProcessPoolExecutor / as_completed；executor.submit
- os.sched_getaffinity(0)（可用 CPU 数）
- fastwarc.warc.ArchiveIterator / WarcRecordType
- tldextract.TLDExtract（按域名过滤/统计）
- tqdm；collections.Counter

边界情况:
- 过滤顺序影响吞吐: 规则先行，fastText 分类器殿后（贵）
- 分类器模型在每个子进程加载一次（进程池 initializer），
  不要每条文档加载
- 阈值是主要调参空间: 太严数据不够训练（需 ~8.6B tokens 采样），
  太松质量差
- 域名分布可作特征: C4-100-domains 是目标分布，可向其靠拢
- 千万别把 Paloma 验证集文本混进训练数据（红线）
- 单文件失败不要拖垮整个管线（try/except + 记日志）

测试:
- 无 pytest；自检:
- 每步统计是否合理（没有某步把 99% 都丢了/全都留了）
- 抽查最终数据 5 条 + 被丢弃数据 5 条，人工判断过滤是否合理
  （这是 inspect_filtered_data 的交付）
- 总产出 token 量是否支撑训练采样规模
```
