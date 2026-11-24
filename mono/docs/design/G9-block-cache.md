# G9 块设备与缓冲管理

- 王道：4.1 文件系统基础（缓冲思想）；4.3 文件系统层次
- 汤小丹：第 8 章 文件管理（I/O 接口、缓冲层）

## 实现

- 块大小 4096 B，RAM 盘 256 块（1 MiB），静态内存。
- `struct buf`：块号、数据、valid/dirty、最近使用计数；16 个缓冲区。
- `bread`：命中直接返回并增加 `hit`；未命中选空闲或最久未用的缓冲区，必要时回写脏块，再从 RAM 盘读入，增加 `miss`。
- `bwrite` 立即写回 RAM 盘（写穿透，简化一致性）。
- `brelse` 为空操作；单核且没有并发块 I/O。

## 取舍

- 不做 virtio-blk、真实磁盘、请求合并、异步 I/O。
- LRU 只在 16 个缓冲区里选择，不维护精确链表。
- `bio_selftest` 用块 200 写 `OSLAB-BIO` 并读回两次。

## 验收

```bash
make && timeout 10s make run
# [bio] pattern rw ok; hit=1 miss=1
```

## 已知限制

- 没有块设备抽象层（G11 只做字符设备 devsw）。
- RAM 盘不持久化，重启即格式化。
