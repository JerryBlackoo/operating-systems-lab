# 17 · 数据加载（data_loading）

讲义 §5.1 ｜ 2 分 ｜ adapter: `adapters.run_get_batch` ｜ 测试: `uv run pytest -k test_get_batch`

```text
Problem:
get_batch — 从一维 token 序列随机采样 (inputs, targets) 批（纯函数）

输入:
x: np.ndarray        一维整数 token ID 数组（可能是 np.memmap，dtype uint16）
batch_size: int      B
context_length: int  m
device: str          'cpu' / 'cuda:0' / 'mps'

输出:
(inputs, targets) 两个 torch 张量，shape 都是 (batch_size, context_length)，
放在指定 device 上；targets 是 inputs 整体右移一位的下一个 token

参数:
无

公式:
起点 i ~ Uniform{0, ..., n − m − 1}      # 保证 i+m 位置还有 target
inputs  = x[i : i+m]
targets = x[i+1 : i+m+1]

步骤:
1. starts = np.random.randint(0, len(x) − context_length, size=batch_size)
2. inputs  = np.stack([x[i : i+m]     for i in starts])
3. targets = np.stack([x[i+1 : i+m+1] for i in starts])
4. 转 torch: torch.from_numpy(arr.astype(np.int64))
5. .to(device) 后返回

需要查的 API:
- np.random.randint(low, high, size)     # high 不含
- np.memmap / np.load(..., mmap_mode='r')   # 大数据必须内存映射加载
- np.stack；ndarray.astype(np.int64)
- torch.from_numpy / Tensor.to(device)

边界情况:
- 起点上界是 n − m（不含），否则 targets 越界
- dtype 链: 磁盘 uint16 -> torch 不支持 uint16，必须先 astype(int64) 再转张量
- memmap 切片是惰性的，np.stack/astype 会实际拷贝这一小块（这正是想要的）
- targets 与 inputs 是同一段错位 1，不是独立采样
- device 是字符串，'mps'（Mac）也要能用
- 训练/验证数据用 np.load(mmap_mode='r') 打开，防止整个数据集读进内存

测试:
- 两个输出 shape 都是 (B, m)？dtype 是 long？在正确的 device 上？
- 对齐检查: 固定随机种子后，targets[:, :-1] == inputs[:, 1:]？
- 所有值 < vocab_size？
- uv run pytest -k test_get_batch
```
