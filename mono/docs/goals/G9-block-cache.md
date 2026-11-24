# [G9] 块设备与缓冲管理

**依赖**：G1  
**建议 Agent**：A6

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 4.1 文件系统基础（缓冲思想） | 第 8 章 文件管理（I/O 接口） |
| 4.3 文件系统层次 | 内核缓冲层 |

## 目标

1. **块大小** 4096 B；`struct buf` + LRU 或简易链表缓存。
2. 后端：**RAM 磁盘** 固定内存区域或 QEMU virtio-blk（RISC-V 优先）；ARM 可仅 RAM 盘。
3. API：`bread` / `bwrite` / `brelse`；`sync` 刷脏。
4. 命中率计数 `cache_hit` / `cache_miss`。

## 交付

- `kernel/fs/bio.c`, `kernel/fs/ramdisk.c` 或 `virtio_blk.c`
- `docs/design/G9-block-cache.md`

## 验收

```bash
make && make run
# [bio] pattern rw ok; hit=... miss=...
```

对块 0 写入魔数，读回一致；连续读同一块两次 hit 增加。

## 非目标

- 多磁盘、分区表、GPT

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。