# ext4 server

ext4 的**盘上格式与元数据管理**。只懂 ext4 的数据结构，不懂路径语义（归 vfs），也不懂块队列与调度（归 block）。

## 最小模型

模型从干净的只读 ext4 镜像读取一个目录项及其文件内容。测试镜像使用 4 KiB 文件系统块、32 字节的普通块组描述符、采用 `filetype` 的线性目录，以及直接存于 inode 的 extent 叶（`eh_depth = 0`）。块读取由内存镜像替身提供。

```text
Mount  = (image, superblock, group_descriptors, root_inode, state)
Extent = (file_block_start, fs_block_start, count, initialized)

UNMOUNTED ──mount 校验通过──> MOUNTED ──unmount──> UNMOUNTED
                               ├──lookup──> inode 与类型
                               └──read_at──> 文件字节
```

| 操作 | 契约 |
| --- | --- |
| `mount(image)` | 从镜像起点的字节偏移 1024 读主超级块，校验魔数 `0xEF53`、干净状态（`s_state = 1`）、4 KiB 块与相同簇大小、32 字节组描述符（`s_desc_size` 为 0 或 32）、必需特性 `filetype` 与 `extents`（容忍 `flex_bg`）、只读兼容特性掩码和几何，装入组描述符并验证根目录 inode 2，再发布挂载状态。[保留 inode 编号](https://docs.kernel.org/filesystems/ext4/special_inodes.html) |
| `lookup(dir_inode, name)` | 读取目录的线性目录项，按单个名字查出 inode 号，再从 inode 确认类型 |
| `read_at(inode, offset, len)` | 以文件大小裁剪读取范围，用 extent 查找每个文件逻辑块对应的文件系统块。对文件空洞和未初始化 extent 的对应字节填 0 |
| `unmount()` | 释放本次挂载持有的元数据与镜像引用 |

在上述块组布局下，主超级块位于文件系统块 0 内，主组描述符表从块 1 开始。 [块组布局](https://docs.kernel.org/filesystems/ext4/blockgroup.html) · [超级块](https://docs.kernel.org/filesystems/ext4/super.html)

定位 inode 时把 `ino - 1` 除以每组 inode 数，商是组号，余数是组内索引。把组描述符给出的 inode 表起始块号换算为字节偏移，再加上组内索引乘 inode 大小，即为表项的字节偏移。 [组描述符](https://docs.kernel.org/filesystems/ext4/group_descr.html) · [inode 表](https://docs.kernel.org/filesystems/ext4/inodes.html)

读取数据先用文件偏移除以文件系统块大小，得到文件逻辑块号和块内偏移。命中 extent 时，`文件系统块号 = extent.fs_block_start + (文件逻辑块号 - extent.file_block_start)`。在 512 字节逻辑块的设备上，一个 4 KiB 文件系统块对应从 `文件系统块号 * 8` 起的 8 个镜像相对 LBA。 [extent 格式](https://docs.kernel.org/filesystems/ext4/ifork.html)

## 不变量

1. 挂载只在元数据检查通过后发布。超级块的魔数、状态、块与簇大小、描述符大小、特性位和几何必须先通过；每个块组的 inode 表必须落在镜像声明的块范围内；根 inode 必须是大小按块对齐的目录。读取 inode 前检查编号范围。每次读盘的块范围同时受超级块声明的总块数和镜像长度约束。
2. 目录项的 `rec_len` 按 4 字节对齐，能包住条目头和名字，且推进后仍在当前块内。跳过 inode 号为 0 的空目录项。 [线性目录格式](https://docs.kernel.org/filesystems/ext4/directory.html)
3. 同一文件的 extent 逻辑区间互不重叠。成功读取的范围止于 `i_size`，偏移到达文件末尾时返回空数据。损坏的目录项或 extent 返回格式错误。
4. inode 必须带 `extents` 标志且不带 `inline_data`。`ee_len` 为 `0x8000` 时按 32768 个已初始化块处理，大于 `0x8000` 时低 15 位是未初始化长度。 [extent 格式](https://docs.kernel.org/filesystems/ext4/ifork.html)

## 独立验证

在内存镜像中放一个两块文件，把两个文件逻辑块映射到不相邻的文件系统块。查到 inode 后执行 `read_at(inode, 4094, 4)`，核对结果依次来自第一块末尾和第二块开头。再加入文件空洞、末尾短读以及 `rec_len = 0` 的目录项，分别检查填零、截断与格式错误。

## QEMU demo

`ext4.c` 解析只读字节镜像。实现使用 4 KiB 块、32 字节组描述符和至多 8 个块组。demo 构造两个块组和跨两个目录块的文件，将目录与目标文件放在不同组，检查非连续 extent、空洞和未初始化 extent；再逐项破坏魔数、状态、块/簇大小、描述符大小、特性位、几何、inode 表范围和根 inode，核对挂载拒绝路径，并覆盖 inode 0 目录项、`rec_len` 未对齐和 extent 重叠。

挂载不验证 `gdt_csum` / `metadata_csum` 校验和：只读模型忽略这些只读兼容特性，但保留全部结构边界检查。

```sh
make -C micro/userland/servers/fs/ext4 run
```

从仓库根目录运行，成功时输出 `[ext4] PASS` 并退出 QEMU。

## 职责

| 文件（规划） | 职责 | Linux 对应 |
| --- | --- | --- |
| `super.c` | 超级块解析、挂载时的组描述符装载 | `fs/ext4/super.c` |
| `balloc.c` | 块位图读取、单个空闲块分配 | `fs/ext4/balloc.c` |
| `mballoc.c` | 多块 / 伙伴式预分配 | `fs/ext4/mballoc.c` |
| `ialloc.c` | inode 位图与 inode 分配 | `fs/ext4/ialloc.c` |
| `inode.c` | inode 读写、间接块 / extent 解析 | `fs/ext4/inode.c` |
| `dir.c` | 目录项查找、插入、删除 | `fs/ext4/dir.c` |
| `file.c` | 按 inode 偏移读写，拆成块请求 | `fs/ext4/file.c` |

## 不归本目录

- 「`/home/a.txt` 是哪一路径」→ `servers/vfs/`
- 「第 N 块和第 N+1 块能不能合并、谁先发」→ `servers/block/`
- 「NVMe 提交队列怎么写」→ `servers/drivers/storage/nvme/`

## 边界判据

问：换一个文件系统（FAT32 / btrfs），这段代码还需要吗？

- 需要 → 不是 ext4 的事
- 不需要（只有 ext4 这样组织盘上数据）→ 本目录

## 从 Linux 迁移时要改的三处接口

以 `balloc.c` 为例，算法逻辑可直接沿用，接口必须替换：

| Linux 原接口 | 微内核替身 |
| --- | --- |
| `sb_getblk(sb, blk)` | 向 block server 发请求消息 |
| `ext4_read_bh_nowait(...)` | 异步请求 + 完成消息回调 |
| `wait_on_buffer(bh)` | 阻塞等待 IPC 回复 |

`ext4_get_group_desc()` 这类**纯内存查表**无需改动——它依赖挂载时已装载的组描述符数组，不产生 I/O。

## 与 vfs 的分工

vfs 下发的是「inode + 偏移 + 长度」，本目录负责翻译成「块号 + 块数」并维护 ext4 自己的元数据一致性（位图、组描述符计数、校验和）。
