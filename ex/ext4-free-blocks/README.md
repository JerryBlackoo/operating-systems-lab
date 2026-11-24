# ext4 空闲块管理：从教材位示图到本机 mballoc

对应 **2027 王道 第 4 章 4.2.6 / 物理结构 · 存储空间管理**，把「位示图 / 空闲链表 / 成组链接」这条纸面知识，落到你这台小新 Pro 14 IRH8（Ubuntu 24.04 HWE，内核 **6.14.0-37-generic**，根分区 `/dev/nvme0n1p7` = **ext4**）的真实代码上。

> 一句话主线：**盘上仍是位图（bitmap），但内核在位图之上又建了一层 buddy 空闲链 + 预分配，用来换「碎片少、顺序写快」。**
> 408 答题口径不变：写「位示图法 + 块组描述符记空闲数」；被追问 Linux 实况，再补 mballoc。

---

## 0. 先看本机是什么文件系统

```
/dev/nvme0n1p7  /              ext4     rw,relatime
/dev/nvme0n1p1  /boot/efi      vfat     —— FAT 簇表
/dev/nvme0n1p5  /media/jerry/Data  ntfs3  —— NTFS $Bitmap
loop0..15       /snap/*        squashfs —— 只读，无动态分配
```

采集证据见 [`evidence/2026-11-23-machine.txt`](evidence/2026-11-23-machine.txt)，要点：

| 项 | 值 | 含义 |
|---|---|---|
| 块大小 | 4096 B | 一个位图块可描述 4096×8 = **32768 个块** |
| 总块数 | 28,392,757 | ≈ 108 GiB |
| 空闲块 | 6,426,053 | 其中 1,453,811 被 root 预留（`可用 4,972,242`） |
| `reserved_clusters` | 4096 | 超级用户保留 ~16 MB |
| `mb_group_prealloc` | 512 | 组预分配默认块数 |
| `mb_stream_req` | 16 | ≤16 块的请求走「小文件组预分配」 |
| `mb_max_to_scan` | 200 | 一次分配最多扫 200 个块组 |
| `mb_order2_req` | 2 | ≤4 块的请求走 order-2 快速路径 |
| `delayed_allocation_blocks` | 329 | 当前尚未落盘的延迟分配块数（≈1.3 MB） |

`stat -f /` 里那个 `类型：ext2/ext3` 是 glibc 的旧命名（`EXT2_SUPER_MAGIC` 也被 ext4 复用），不代表真是 ext2。

---

## 1. 盘上结构：块组 + 位图 + 组描述符

ext4 把整个分区切成若干 **块组（block group）**，每组大小 = 8 × 块大小 个块（这里 32768 块 = 128 MiB）。每组固定含：

```
┌──────────────────────── 一个块组 (32768 blocks × 4KiB) ────────────────────────┐
│ 超级块副本 │ 组描述符表 │ 块位图(1块) │ inode位图(1块) │ inode表 │  数据块…    │
│ (仅部分组) │ (仅部分组) │ 32768 bit   │               │         │             │
└────────────────────────────────────────────────────────────────────────────────┘
```

块位图里 **1 bit = 1 个块**（开启 `bigalloc` 时是 1 个簇）：`0` 空闲、`1` 已占用。

对应的盘上数据结构（`kernel-source/ext4.h`）：

```395:420:kernel-source/ext4.h
struct ext4_group_desc
{
	__le32	bg_block_bitmap_lo;	/* Blocks bitmap block */
	__le32	bg_inode_bitmap_lo;	/* Inodes bitmap block */
	__le32	bg_inode_table_lo;	/* Inodes table block */
	__le16	bg_free_blocks_count_lo;/* Free blocks count */
	__le16	bg_free_inodes_count_lo;/* Free inodes count */
	__le16	bg_used_dirs_count_lo;	/* Directories count */
	__le16	bg_flags;		/* EXT4_BG_flags (INODE_UNINIT, etc) */
	// … 校验和与 hi 字段（支持 >2^32 块）…
};
```

要点：

- **位图块地址**存在组描述符 `bg_block_bitmap_lo/hi`，**空闲计数**存在 `bg_free_blocks_count_lo/hi`。
- 组描述符在挂载时全部读入内存（`ext4_fill_super` → `s_group_desc`），所以「还剩多少空闲块」是**查内存计数**，不是每次都数位图。
- 这就是教材「位示图法」的工程版：位图是事实来源，描述符计数是缓存。

内核注释自己说得很清楚：

```247:256:kernel-source/balloc.c
/*
 * The free blocks are managed by bitmaps. A file system contains several
 * blocks groups. Each group contains 1 bitmap block for blocks, 1 bitmap
 * block for inodes, N blocks for the inode table and data blocks.
 *
 * The file system contains group descriptors which are located after the
 * super block. Each descriptor contains the number of the bitmap block and
 * the free blocks count in the block. The descriptors are loaded in memory
 * when a file system is mounted (see ext4_fill_super).
 */
```

### 1.1 位图初始化是「偷懒」的

新格式化的大分区如果给每个块组的位图都写一遍，`mkfs` 会很慢。所以 ext4 用 `EXT4_BG_BLOCK_UNINIT` 标志：**位图块不写，直接信任「整组除元数据外全空闲」**，第一次真正分配时才初始化：

```4021:4028:kernel-source/mballoc.c
	if (ext4_has_group_desc_csum(sb) &&
	    (gdp->bg_flags & cpu_to_le16(EXT4_BG_BLOCK_UNINIT))) {
		gdp->bg_flags &= cpu_to_le16(~EXT4_BG_BLOCK_UNINIT);
		ext4_free_group_clusters_set(sb, gdp,
			ext4_free_clusters_after_init(sb, group, gdp));
	}
```

`ext4_free_clusters_after_init()` = 组内簇数 − 元数据占用簇数，见 `balloc.c:239`。
`/sys/fs/ext4/nvme0n1p7/` 里能看到 `lazy_itable_init`，inode 表同理。

---

## 2. 内存中多出来的两层：`ext4_group_info` 与 buddy

位图只能回答「第 k 块空不空」，回答不了「**有没有连续 512 块**」。所以 mballoc 在内存里为每个块组维护一个 `ext4_group_info`：

```3400:3424:kernel-source/ext4.h
struct ext4_group_info {
	unsigned long   bb_state;
	struct rb_root  bb_free_root;
	ext4_grpblk_t	bb_first_free;	/* first free block */
	ext4_grpblk_t	bb_free;	/* total free blocks */
	ext4_grpblk_t	bb_fragments;	/* nr of freespace fragments */
	int		bb_avg_fragment_size_order;
	ext4_grpblk_t	bb_largest_free_order;
	ext4_group_t	bb_group;
	struct          list_head bb_prealloc_list;
	struct rw_semaphore alloc_sem;
	struct list_head bb_avg_fragment_size_node;
	struct list_head bb_largest_free_order_node;
	ext4_grpblk_t	bb_counters[];	/* Nr of free power-of-two-block
					 * regions, index is order.
					 * bb_counters[3] = 5 means
					 * 5 free 8-block regions. */
};
```

**`bb_counters[]` 就是「伙伴系统」的直方图**：`bb_counters[3] == 5` 表示本组内有 5 个「8 块对齐的连续空闲区」。

配合 buddy 位图（每阶一张，`order 0..blocksize_bits+2`）：

```
组内 bitmap (32768 bit)            order 0  →  32768 bit
                                    order 1  →  16384 bit（每 2 块合并后是否全空）
                                    order 2  →   8192 bit（每 4 块）
                                    …
                                    order 13 →      4 bit
```

`MB_NUM_ORDERS(sb) == s_blocksize_bits + 2`，4 KiB 块即 **14 阶**（0–13），最大能一次描述 2^13 = 8192 块连续空闲。

`ext4_mb_generate_buddy()` 扫一遍位图，把每个连续空闲段按「能凑出的最大 2 的幂」逐段登记：

```1183:1212:kernel-source/mballoc.c
void ext4_mb_generate_buddy(struct super_block *sb,
				void *buddy, void *bitmap, ext4_group_t group,
				struct ext4_group_info *grp)
{
	// …
	i = mb_find_next_zero_bit(bitmap, max, 0);
	grp->bb_first_free = i;
	while (i < max) {
		fragments++;
		first = i;
		i = mb_find_next_bit(bitmap, max, i);
		len = i - first;
		free += len;
		if (len > 1)
			ext4_mb_mark_free_simple(sb, buddy, first, len, grp);
```

注意最后的一致性检查——**位图与描述符计数不符就报损坏**，并以位图为准：

```1214:1224:kernel-source/mballoc.c
	if (free != grp->bb_free) {
		ext4_grp_locked_error(sb, group, 0, 0,
			"block bitmap and bg descriptor "
			"inconsistent: %u vs %u free clusters",
			free, grp->bb_free);
		// …
		grp->bb_free = free;
```

`ext4_mb_mark_free_simple()` 的核心是「按 2 的幂切分」的贪心，正是教材里伙伴算法的分配/回收对称操作：

```784:816:kernel-source/mballoc.c
static void ext4_mb_mark_free_simple(struct super_block *sb,
		void *buddy, ext4_grpblk_t first, ext4_grpblk_t len,
			struct ext4_group_info *grp)
{
	// …
	while (len > 0) {
		/* find how many blocks can be covered since this position */
		max = ffs(first | border) - 1;

		/* find how many blocks of power 2 we need to mark */
		min = fls(len) - 1;

		if (max < min)
			min = max;
		chunk = 1 << min;

		/* mark multiblock chunks only */
		grp->bb_counters[min]++;
		if (min > 0)
			mb_clear_bit(first >> min,
				     buddy + sbi->s_mb_offsets[min]);
		len -= chunk;
		first += chunk;
	}
}
```

`ffs(first | border) - 1` 算的是「从 first 起按对齐能走多远」，`fls(len) - 1` 是「剩余长度能凑的最大幂」，取小者切一块。**这就是「伙伴」二字的全部含义：地址对齐 + 长度成幂**。

---

## 3. 分配路径：`ext4_mb_new_blocks`

大文件写（> `mb_stream_req`=16 块）走流式预分配；小文件走组预分配。入口：

```6133:6200:kernel-source/mballoc.c
ext4_fsblk_t ext4_mb_new_blocks(handle_t *handle,
				struct ext4_allocation_request *ar, int *errp)
{
	// …
	/* Without delayed allocation we need to verify
	 * there is enough free blocks to do block allocation
	 * and verify allocation doesn't exceed the quota limits.
	 */
	while (ar->len &&
		ext4_claim_free_clusters(sbi, ar->len, ar->flags)) {
		/* let others to free the space */
		cond_resched();
		ar->len = ar->len >> 1;
	}
	if (!ar->len) {
		ext4_mb_show_pa(sb);
		*errp = -ENOSPC;
		return 0;
	}
```

三处值得记住：

1. **先扣全局额度** `ext4_claim_free_clusters`（`balloc.c:668`），不够就 `len >>= 1` 降级重试，最后仍不够才 `-ENOSPC`。
2. `cond_resched()` —— 空闲块紧张时让出 CPU，避免把系统卡死。
3. 降半重试是「宁可少要，不要失败」的工程妥协。

`ext4_claim_free_clusters` 判的是 **三个计数器之差**（空闲 / 脏 / 根预留）：

```668:676:kernel-source/balloc.c
int ext4_claim_free_clusters(struct ext4_sb_info *sbi,
			     s64 nclusters, unsigned int flags)
{
	if (ext4_has_free_clusters(sbi, nclusters, flags)) {
		percpu_counter_add(&sbi->s_dirtyclusters_counter, nclusters);
		return 0;
	} else
		return -ENOSPC;
}
```

`ext4_has_free_clusters()` 的完整判断（`balloc.c:620` 起）：

```c
if (free_clusters >= (rsv + nclusters + dirty_clusters))
	return 1;                    /* 普通用户：扣掉根预留和脏块后还够 */
// …
if (free_clusters >= (nclusters + dirty_clusters + resv_clusters))
	return 1;                    /* root：可以动用预留池 */
```

**为什么要有 dirty_clusters**：延迟分配（delalloc）下，数据还在 page cache 没落盘，块已经在「账上」被承诺出去了。不扣这一份就会出现「账面够、真写时不够」。

---

## 4. 真正改位图：`ext4_mb_mark_context`

所有 bitmap 修改最终都收敛到这一个函数（`state=true` 分配、`false` 释放）：

```3977:4046:kernel-source/mballoc.c
ext4_mb_mark_context(handle_t *handle, struct super_block *sb, bool state,
		     ext4_group_t group, ext4_grpblk_t blkoff,
		     ext4_grpblk_t len, int flags, ext4_grpblk_t *ret_changed)
{
	// …
	bitmap_bh = ext4_read_block_bitmap(sb, group);
	// … journal 拿写权限 …
	if (flags & EXT4_MB_BITMAP_MARKED_CHECK) {
		already = 0;
		for (i = 0; i < len; i++)
			if (mb_test_bit(blkoff + i, bitmap_bh->b_data) ==
					state)
				already++;
		changed = len - already;
	}

	if (state) {
		mb_set_bits(bitmap_bh->b_data, blkoff, len);
		ext4_free_group_clusters_set(sb, gdp,
			ext4_free_group_clusters(sb, gdp) - changed);
	} else {
		mb_clear_bits(bitmap_bh->b_data, blkoff, len);
		ext4_free_group_clusters_set(sb, gdp,
			ext4_free_group_clusters(sb, gdp) + changed);
	}

	ext4_block_bitmap_csum_set(sb, gdp, bitmap_bh);
```

这一段把三层一次写完，是整章最该背的代码：

| 动作 | 代码 | 教材对应 |
|---|---|---|
| 改位图比特 | `mb_set_bits` / `mb_clear_bits` | 位示图中「置 1 / 清 0」 |
| 改计数 | `ext4_free_group_clusters_set(±changed)` | 组描述符里的空闲数 |
| 改校验 | `ext4_block_bitmap_csum_set` | `metadata_csum` 特性，防位图被静默改坏 |

**幂等性**：`EXT4_MB_BITMAP_MARKED_CHECK` 会先数「已经是目标状态的位」，只对真正变化的位加减计数。重复释放同一块不会把计数搞错——这是 `ext4_mb_mark_bb()`（`mballoc.c:4156`）给 fast-commit 重放用的幂等入口。

---

## 5. 释放路径：`ext4_free_blocks`

```6558:6591:kernel-source/mballoc.c
void ext4_free_blocks(handle_t *handle, struct inode *inode,
			struct buffer_head *bh, ext4_fsblk_t block,
			unsigned long count, int flags)
{
	// …
	if (sbi->s_mount_state & EXT4_FC_REPLAY) {
		ext4_free_blocks_simple(inode, block, EXT4_NUM_B2C(sbi, count));
		return;
	}

	might_sleep();

	if (!(flags & EXT4_FREE_BLOCKS_VALIDATED) &&
	    !ext4_inode_block_valid(inode, block, count)) {
		ext4_error(sb, "Freeing blocks not in datazone - "
			"block = %llu, count = %lu", block, count);
		return;
	}
	flags |= EXT4_FREE_BLOCKS_VALIDATED;
	// … trace、cluster 边界对齐、journal、mb_free_blocks …
```

释放比分配多三道防线：

1. **范围校验** `ext4_inode_block_valid`——防越界释放（安全/一致性双重要求）。
2. **簇边界对齐** `EXT4_PBLK_COFF`——`bigalloc` 下释放必须以簇为单位，函数会向前/向后扩到簇边界（`mballoc.c:6604` 起）。
3. **journal**——先记日志再改位图，崩溃后重放能恢复。

释放真正落到 buddy 上是在 journal 提交后的 `ext4_free_data_in_buddy()`（`mballoc.c:3859`），它从 `bb_free_root` 红黑树里摘掉 entry 再调 `mb_free_blocks()` **合并相邻伙伴**——这就是「空闲块回收后要尽量合并成大块」的实现位置。

---

## 6. sysfs 调参：本机实际值

```bash
ls /sys/fs/ext4/nvme0n1p7/
cat /sys/fs/ext4/nvme0n1p7/mb_group_prealloc   # 512
cat /sys/fs/ext4/nvme0n1p7/mb_stream_req       # 16
cat /sys/fs/ext4/nvme0n1p7/delayed_allocation_blocks
cat /sys/fs/ext4/nvme0n1p7/mb_stats            # 置 1 后统计生效（有开销）
```

| 旋钮 | 本机 | 作用 |
|---|---|---|
| `mb_group_prealloc` | 512 | 小文件一次性从组里预占多少块（减少锁竞争） |
| `mb_stream_req` | 16 | 文件大于此值视为「流式」，改用 inode 预分配 |
| `mb_max_to_scan` | 200 | 找不到合适块组时最多扫多少个 |
| `mb_min_to_scan` | 10 | 至少扫多少个才承认「没合适的」 |
| `mb_order2_req` | 2 | ≤2^2=4 块的小请求走 order-2 位图快速匹配 |
| `mb_prefetch` | 128 | 预取 buddy 的组数 |

`MB_DEFAULT_GROUP_PREALLOC` 定义在 `mballoc.h:74`，默认值 512，与你机器一致。

---

## 7. 「还剩多少」怎么算：`ext4_count_free_clusters`

```770:830:kernel-source/balloc.c
ext4_fsblk_t ext4_count_free_clusters(struct super_block *sb)
{
	ext4_fsblk_t desc_count;
	struct ext4_group_desc *gdp;
	// …
```

这个函数遍历所有组描述符累加（带 `EXT4FS_DEBUG` 时还会真数位图做交叉验证）。用户态等价物：

```bash
stat -f /            # 最省事：块总数 / 空闲 / 可用
sudo tune2fs -l /dev/nvme0n1p7 | rg 'Block count|Free blocks|Block size|Reserved block'
dumpe2fs -h /dev/nvme0n1p7 2>/dev/null | head -30
```

`df` 的「可用」比「空闲」小，差的就是 `reserved_clusters`（本机 4096 簇 ≈ 16 MB）+ 元数据预留。

---

## 8. 与 408 教材的逐条对照

| 教材概念 | ext4 实现 | 差异要点 |
|---|---|---|
| 空闲表法 | ✗ 未使用 | 连续分配才需要，ext4 用 extent |
| 空闲链表法 | ✗ 盘上不用 | 内存 `bb_prealloc_list` 是预分配链，不是盘上空闲链 |
| **位示图法** | ✓ **每块组一张 block bitmap** | 位图分散到各组，不是全盘一张 |
| 成组链接法 | ✗ | UNIX SysV 专用 |
| 块号 ↔ 位号换算 | `块号 = 组首块 + 位号 × 簇大小` | `ext4_get_group_no_and_offset()`，`balloc.c:54` |
| 分配后置 1 | `mb_set_bits` | 同时更新 `bg_free_blocks_count` 与 csum |
| 回收后置 0 | `mb_clear_bits` | 还要合并 buddy 伙伴 |
| 平均访盘次数题 | — | 教材题（如 3200 个 FCB）仍是纸面模型 |

**答题纪律**：408 单选/填空写「位示图」，不要写 mballoc；只有题目明确问 Linux/ext4 才展开 buddy 与预分配。反过来，被问「为什么位图之外还要 buddy」时，答：位图 O(n) 找连续区，buddy 用 `bb_counters[]` 直方图 O(1) 判断「是否存在 ≥k 阶连续空闲」，且天然抗碎片。

---

## 9. 动手验证清单

```bash
cd /home/jerry/Projects/from-scratch/operating-systems-lab/ex/ext4-free-blocks

# ① 看本机实况（已采集，可重跑）
bash collect.sh

# ② 观察一次分配对位图的真实影响
#   先记下 baseline，再写 1 GiB 文件，再看差异
cat /sys/fs/ext4/nvme0n1p7/delayed_allocation_blocks
dd if=/dev/zero of=/tmp/ext4-test.bin bs=1M count=1024 status=progress
cat /sys/fs/ext4/nvme0n1p7/delayed_allocation_blocks   # 涨到 ~26 万
sync
cat /sys/fs/ext4/nvme0n1p7/delayed_allocation_blocks   # 回落
rm /tmp/ext4-test.bin && sync

# ③ 用 filefrag 看 extent 是否连续（预分配效果）
filefrag -v /tmp/ext4-test.bin 2>/dev/null   # 删除前执行

# ④ 看某文件在哪个块组
stat -c '%n ino=%i blocks=%b blksize=%B' /etc/passwd
sudo debugfs -R "stat <$(stat -c %i /etc/passwd)>" /dev/nvme0n1p7 2>/dev/null | head -20
```

`filefrag` 输出的 extent 数量是最直观的「碎片」证据：512 块组预分配生效时，1 GiB 文件应该只有少数几个 extent，而不是几千个。

---

## 10. 目录说明

```
ext4-free-blocks/
├── README.md                     ← 本讲义
├── collect.sh                    ← 一键采集本机证据
├── evidence/
│   └── 2026-11-23-machine.txt    ← 本机实况快照（可复现）
└── kernel-source/                ← Linux v6.14 ext4 源码摘录（md5 见 evidence）
    ├── balloc.c                  ← 组描述符、空闲统计、claim
    ├── mballoc.c                 ← buddy、分配/释放主逻辑
    ├── mballoc.h                 ← ext4_buddy、预分配常量
    ├── bitmap.c                  ← 位图 csum 校验
    ├── ialloc.c                  ← inode 位图（对照块位图）
    └── ext4.h                    ← ext4_group_desc、ext4_group_info
```

源码取自 `https://github.com/torvalds/linux` tag **v6.14**，与本机 `6.14.0-37-generic` 大版本一致（Ubuntu 有少量补丁，见 `ex/elf-linking/kernel-source/ubuntu-selected.patch` 的同类做法）。

## 11. 阅读顺序（建议 40 分钟）

1. `balloc.c` 247–256 —— 建立「块组 = 2 张位图 + inode 表 + 数据块」的图
2. `ext4.h` 395–420 —— 组描述符字段
3. `ext4.h` 3400–3424 —— `bb_counters[]` 的语义（**最关键**）
4. `mballoc.c` 1183–1230 —— buddy 如何从位图生成
5. `mballoc.c` 784–816 —— 幂次切分
6. `mballoc.c` 6133–6200 —— 分配入口与降半重试
7. `mballoc.c` 3977–4046 —— 位图三层同改
8. `mballoc.c` 6558–6591 —— 释放入口

## 12. 下一步可做

- 用 `bpftrace` 挂 `ext4_mb_new_blocks` 统计 order 分布，验证「小文件真走组预分配」
- 造碎片：交替写 1 MB / 删一半 / 再写，用 `filefrag` 对比 extent 数
- 读 `fs/ext4/ialloc.c` 把 inode 位图与块位图的对称性补全
- 对照 `ext4_mb_normalize_request()`（`mballoc.c` 内）看「预分配长度怎么被规整成 2 的幂」
