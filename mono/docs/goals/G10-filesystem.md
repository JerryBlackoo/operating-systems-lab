# [G10] 简易可读写文件系统

**依赖**：G9  
**建议 Agent**：A6

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 4.2 目录与文件 | 第 8 章 目录结构、索引分配 |
| 4.3 文件系统 | 空闲空间管理 |

## 目标

1. 单文件系统：superblock + inode 表 + 数据区位图 + 根目录。
2. 路径解析 `/foo/bar`；固定文件名长度（如 28 字节）。
3. syscall：`open` `read` `write` `close` `mkdir`（可选 `ls` 内核辅助）。
4. 镜像生成工具 `tools/mkfs.py` 打包 `userland/motd.txt` 进磁盘镜像。

## 交付

- `kernel/fs/fs.c`, `inode.c`, `dir.c`
- `tools/mkfs.py`
- `docs/design/G10-filesystem.md`：与王道 i-node 图对照

## 验收

```bash
make && make run
# 用户态 cat /motd 或内核内置测试读出预定字符串
# echo hello > /tmp 若实现写
```

## 非目标

- ext4、日志、权限 ACL、硬链接

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。