# [G12] 磁盘请求调度（SCAN / C-SCAN）

**依赖**：G11、G9  
**建议 Agent**：A7

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 5.3 磁盘和固态硬盘 | 磁盘访问调度 |

## 目标

1. 在块 I/O 提交队列上实现 **SCAN** 或 **C-SCAN**（二选一，design 说明）。
2. 柱面号由 LBA 线性映射（教学简化：`cylinder = lba / 8` 等，写进文档）。
3. 固定请求序列自测：输出服务顺序，与王道 **5.3 例题** 或自拟表一致。

## 交付

- `kernel/fs/disk_sched.c`
- `docs/design/G12-disk-sched.md`：含例题输入输出表

## 验收

```bash
make && make run
# [disk] scan order: 98 183 37 122 ... (示例)
```

## 非目标

- SSD FTL、NCQ、真实寻道延迟模型

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。