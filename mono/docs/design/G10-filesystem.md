# G10 简易可读写文件系统

- 王道：4.2 目录与文件；4.3 文件系统
- 汤小丹：第 8 章 目录结构、索引分配、空闲空间管理

## 磁盘布局（1 MiB RAM 盘）

| 块 | 内容 |
|---|---|
| 0 | superblock（magic、块数、inode 数、位图/表/数据起点） |
| 1 | inode 位图 |
| 2 | 数据块位图 |
| 3 | inode 表（64 × 40 B） |
| 4..255 | 数据块（4 KiB） |

- inode：`type`（0 空/1 文件/2 目录）、`nlink`、`size`、`direct[8]`；直接索引最多 32 KiB。
- 目录项：`name[28]` + `ino`，每块 128 项。
- 路径解析：从 inode 0 开始逐级 `dir_lookup`；支持绝对路径 `/motd`、`/tmp/note`。
- `fs_create/fs_mkdir/fs_read/fs_write/fs_readdir`；`fs_selftest` 读写 `/tmp/note`。
- `tools/mkfs.py` 生成与内核布局一致的离线 `ramdisk.img`，`motd` 来自 `userland/motd.txt`；内核启动时在内存中格式化同样的布局与内容，当前不读取该文件。

## 与教材 i-node 的对应

- 目录是“文件名 → inode 号”的映射，inode 保存类型、大小和直接块号。
- 位图管理空闲 inode/数据块；没有空闲块成组链接。
- 没有间接块、日志、权限、硬链接、符号链接、ext4。

## 验收

```bash
make && timeout 20s make run
# [fs] /motd: Welcome to operating-systems-lab
# This is a teaching OS on RISC-V and ARM Zynq-7010.
# [fs] rw test: hello

printf 'cat /motd\n' | ... # 交互 shell 中读到同一内容
```

## 已知限制

- 目录项删除/文件截断未实现；文件只增不减。
- 8 个直接块用完即写入失败。
- RAM 盘不持久化。
