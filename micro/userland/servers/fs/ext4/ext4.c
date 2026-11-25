#include "ext4.h"
#include "demo.h"

enum {
    SUPER_OFFSET = 1024, EXT4_MAGIC = 0xef53, GROUP_DESC_SIZE = 32,
    INCOMPAT_FILETYPE = 0x2, INCOMPAT_EXTENTS = 0x40, INCOMPAT_FLEX_BG = 0x200,
    RO_FEATURES = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40 | 0x100 | 0x400,
    INODE_EXTENTS = 0x80000, INODE_INLINE_DATA = 0x10000000,
    EXTENT_MAGIC = 0xf30a, EXTENT_HEAD_LEN = 12, EXTENT_ENTRY_LEN = 12,
    /* i_block 只有 60 字节，内联 extent 最多 4 项。 */
    EXTENT_INLINE_MAX = 4,
    /* 叶块容量由 4 KiB 块减去 12 字节头部，再除以 12 字节目录项得到。 */
    EXTENT_LEAF_MAX = (EXT4_BLOCK - EXTENT_HEAD_LEN) / EXTENT_ENTRY_LEN,
    EXTENT_UNWRITTEN = 0x8000,
    TYPE_MASK = 0xf000, MODE_DIR = 0x4000, MODE_FILE = 0x8000,
};

struct extent {
    uint32_t logical;
    uint64_t physical;
    uint32_t count;
    bool initialized;
};

struct extent_index {
    uint32_t logical;
    uint64_t leaf;
};

struct inode {
    enum ext4_type type;
    uint64_t size;
    uint16_t depth;
    uint16_t count; /* depth == 0: extent 数；depth == 1: 索引项数 */
    struct extent extents[EXTENT_INLINE_MAX];
    struct extent_index indexes[EXTENT_INLINE_MAX];
};

static uint16_t le16(const uint8_t *p)
{
    return (uint16_t)p[0] | (uint16_t)p[1] << 8;
}

static uint32_t le32(const uint8_t *p)
{
    return (uint32_t)le16(p) | (uint32_t)le16(p + 2) << 16;
}

static bool ext4_backed(const struct ext4 *fs)
{
    return fs->image != NULL || fs->io != NULL;
}

static enum ext4_result load_block(const struct ext4 *fs, uint64_t number, uint8_t *buf)
{
    if (fs->image) {
        if (number >= fs->blocks) {
            return EXT4_FORMAT;
        }
        memcpy(buf, fs->image + number * EXT4_BLOCK, EXT4_BLOCK);
        return EXT4_OK;
    }
    if (!fs->io || !fs->io->read_block) {
        return EXT4_NOT_MOUNTED;
    }
    return fs->io->read_block(fs->io->ctx, number, buf);
}

static const uint8_t *block(struct ext4 *fs, uint64_t number)
{
    if (fs->image) {
        return fs->image + number * EXT4_BLOCK;
    }
    if (!fs->io) {
        return NULL;
    }
    if (!fs->cache_valid || fs->cache_block != number) {
        if (load_block(fs, number, fs->cache_blk) != EXT4_OK) {
            return NULL;
        }
        fs->cache_block = number;
        fs->cache_valid = true;
    }
    return fs->cache_blk;
}

/*
 * 解析并校验一个 depth == 0 的 extent 节点（inode 内联根或叶块）。
 *
 * low/high 是该节点负责的文件逻辑块区间：叶块的区间由索引项界定，
 * 内联根覆盖整个文件。prev_end 跨节点累计，用于拒绝跨叶/跨 extent 重叠。
 * 允许空洞（logical > prev_end），但不允许倒序或越界。
 */
static enum ext4_result parse_extent_entries(const struct ext4 *fs, const uint8_t *node,
                                             unsigned capacity, uint64_t low, uint64_t high,
                                             uint64_t *prev_end, struct extent *out,
                                             unsigned out_cap, bool allow_empty)
{
    uint16_t count = le16(node + 2);
    uint16_t max = le16(node + 4);
    uint16_t depth = le16(node + 6);
    uint64_t end = *prev_end;

    if (le16(node) != EXTENT_MAGIC || depth != 0) {
        return EXT4_FORMAT;
    }
    if (count > max || max > capacity || (!allow_empty && count == 0) ||
        (out && count > out_cap)) {
        return EXT4_FORMAT;
    }

    for (unsigned i = 0; i < count; i++) {
        const uint8_t *entry = node + EXTENT_HEAD_LEN + EXTENT_ENTRY_LEN * i;
        uint32_t logical = le32(entry);
        uint16_t raw_len = le16(entry + 4);
        uint64_t physical = (uint64_t)le16(entry + 6) << 32 | le32(entry + 8);
        uint32_t length = raw_len > EXTENT_UNWRITTEN ? raw_len - EXTENT_UNWRITTEN : raw_len;

        if (logical < end || logical < low ||
            (high != UINT64_MAX && (uint64_t)logical + length > high) ||
            !length || !physical || physical >= fs->blocks ||
            length > fs->blocks - physical ||
            (uint64_t)logical + length > (UINT64_C(1) << 32)) {
            return EXT4_FORMAT;
        }
        if (out) {
            out[i] = (struct extent){
                .logical = logical,
                .physical = physical,
                .count = length,
                .initialized = raw_len <= EXTENT_UNWRITTEN,
            };
        }
        end = (uint64_t)logical + length;
    }
    *prev_end = end;
    return EXT4_OK;
}

static enum ext4_result parse_extents(const struct ext4 *fs, const uint8_t *ext,
                                      struct inode *inode)
{
    uint16_t count = le16(ext + 2);
    uint16_t max = le16(ext + 4);
    uint16_t depth = le16(ext + 6);
    uint64_t prev_end = 0;

    if (le16(ext) != EXTENT_MAGIC) {
        return EXT4_FORMAT;
    }
    if (depth > 1) {
        return EXT4_UNSUPPORTED;
    }

    inode->depth = depth;
    inode->count = 0;

    if (depth == 0) {
        enum ext4_result result = parse_extent_entries(fs, ext, EXTENT_INLINE_MAX, 0,
                                                       UINT64_MAX, &prev_end,
                                                       inode->extents, EXTENT_INLINE_MAX,
                                                       true);
        if (result != EXT4_OK) {
            return result;
        }
        inode->count = count;
        return EXT4_OK;
    }

    if (!count || count > max || max > EXTENT_INLINE_MAX) {
        return EXT4_FORMAT;
    }

    const uint8_t *idx = ext + EXTENT_HEAD_LEN;
    for (unsigned i = 0; i < count; i++) {
        const uint8_t *entry = idx + EXTENT_ENTRY_LEN * i;
        uint32_t logical = le32(entry);
        /* extent_idx: ei_block / ei_leaf_lo / ei_leaf_hi / ei_unused */
        uint64_t leaf = (uint64_t)le16(entry + 8) << 32 | le32(entry + 4);

        if (i != 0 && logical <= inode->indexes[i - 1].logical) {
            return EXT4_FORMAT;
        }
        if (!leaf || leaf >= fs->blocks) {
            return EXT4_FORMAT;
        }
        inode->indexes[i] = (struct extent_index){.logical = logical, .leaf = leaf};
    }

    /*
     * 遍历全部索引项：每个叶块覆盖 [index[i].logical, index[i+1].logical)。
     * 叶块自己的 extent 可以稀疏，但不得越过下一个索引项的逻辑边界。
     */
    for (unsigned i = 0; i < count; i++) {
        uint64_t low = inode->indexes[i].logical;
        uint64_t high = (i + 1 < count) ? inode->indexes[i + 1].logical : UINT64_MAX;
        /*
         * 走 fs 的单块 cache，而不是在栈上开 4 KiB 叶块：FS 线程的内核栈
         * 只有 8 KiB，syscall 路径上已经带了 1 KiB 读缓冲和 inode 结构。
         */
        const uint8_t *leaf = block((struct ext4 *)fs, inode->indexes[i].leaf);

        if (leaf == NULL) {
            return EXT4_FORMAT;
        }
        if (le16(leaf) != EXTENT_MAGIC) {
            return EXT4_FORMAT;
        }
        if (le16(leaf + 6) != 0) {
            return EXT4_UNSUPPORTED;
        }
        enum ext4_result got =
            parse_extent_entries(fs, leaf, EXTENT_LEAF_MAX, low, high, &prev_end,
                                 NULL, 0, false);
        if (got != EXT4_OK) {
            return got;
        }
    }
    inode->count = count;
    return EXT4_OK;
}

static enum ext4_result load_inode(const struct ext4 *fs, uint32_t ino, struct inode *inode)
{
    if (!ino || ino > fs->inodes)
        return EXT4_NOT_FOUND;
    uint32_t group = (ino - 1) / fs->inodes_per_group;
    uint32_t slot = (ino - 1) % fs->inodes_per_group;
    /*
     * inode 表通常大于一个块（32M 镜像就有 2 MiB）。块设备路径的
     * block() 只返回单块 cache，因此必须按 slot*inode_size 定位到
     * 真正包含该 inode 的块；否则 inode slot >= 16 会越界读 cache。
     */
    uint64_t inode_off = (uint64_t)slot * fs->inode_size;
    uint64_t table_block = fs->inode_tables[group] + inode_off / EXT4_BLOCK;
    uint32_t within = (uint32_t)(inode_off % EXT4_BLOCK);
    const uint8_t *table = block((struct ext4 *)fs, table_block);
    if (!table) {
        return EXT4_FORMAT;
    }
    const uint8_t *p = table + within;
    uint16_t mode = le16(p) & TYPE_MASK;
    if (mode != MODE_DIR && mode != MODE_FILE)
        return EXT4_WRONG_TYPE;
    uint32_t flags = le32(p + 0x20);
    if (!(flags & INODE_EXTENTS) || (flags & INODE_INLINE_DATA))
        return EXT4_UNSUPPORTED;
    inode->type = mode == MODE_DIR ? EXT4_DIRECTORY : EXT4_FILE;
    inode->size = (uint64_t)le32(p + 0x6c) << 32 | le32(p + 4);
    inode->depth = 0;
    inode->count = 0;

    return parse_extents(fs, p + 0x28, inode);
}

static enum ext4_result mount_super(struct ext4 *fs, const uint8_t *super, uint64_t max_blocks)
{
    if (le16(super + 0x38) != EXT4_MAGIC || le16(super + 0x3a) != 1) {
        return EXT4_FORMAT;
    }
    uint32_t log_block = le32(super + 0x18);
    uint16_t desc_size = le16(super + 0xfe);
    uint32_t incompat = le32(super + 0x60);
    if (log_block != 2 || le32(super + 0x14) != 0 || le32(super + 0x1c) != log_block ||
        (desc_size && desc_size != GROUP_DESC_SIZE) ||
        (incompat & (INCOMPAT_FILETYPE | INCOMPAT_EXTENTS)) !=
            (INCOMPAT_FILETYPE | INCOMPAT_EXTENTS) ||
        /*
         * 不包含 INCOMPAT_RECOVER (0x4)：该位表示 journal 待重放。
         * 本实现只读且不重放 journal，挂载脏卷会静默返回不一致数据，
         * 因此宁可拒绝挂载（EXT4_UNSUPPORTED）。
         */
        (incompat & ~(INCOMPAT_FILETYPE | INCOMPAT_EXTENTS | INCOMPAT_FLEX_BG)) ||
        (le32(super + 0x64) & ~RO_FEATURES)) {
        return EXT4_UNSUPPORTED;
    }

    struct ext4 mounted = *fs;
    mounted.blocks = le32(super + 4);
    mounted.inodes = le32(super);
    mounted.inodes_per_group = le32(super + 0x28);
    mounted.inode_size = le16(super + 0x58);

    uint32_t blocks_per_group = le32(super + 0x20);
    if (mounted.blocks < 2 || mounted.blocks > max_blocks || !blocks_per_group ||
        blocks_per_group > 8 * EXT4_BLOCK || !mounted.inodes_per_group ||
        mounted.inodes_per_group > 8 * EXT4_BLOCK || mounted.inodes < EXT4_ROOT_INODE ||
        mounted.inode_size < 128 || mounted.inode_size > EXT4_BLOCK ||
        (mounted.inode_size & (mounted.inode_size - 1))) {
        return EXT4_FORMAT;
    }
    unsigned groups = 1 + (mounted.blocks - 1) / blocks_per_group;
    if (groups > EXT4_GROUPS) {
        return EXT4_UNSUPPORTED;
    }
    if (mounted.inodes > groups * mounted.inodes_per_group) {
        return EXT4_FORMAT;
    }

    size_t table_size = (size_t)mounted.inodes_per_group * mounted.inode_size;
    size_t table_blocks = (table_size + EXT4_BLOCK - 1) / EXT4_BLOCK;
    {
        const uint8_t *gdt = block(&mounted, 1U);

        if (!gdt) {
            return EXT4_FORMAT;
        }
        for (unsigned i = 0; i < groups; i++) {
            const uint8_t *desc = gdt + i * GROUP_DESC_SIZE;
            uint32_t inode_table = le32(desc + 8);

            if (inode_table <= 1 || inode_table >= mounted.blocks ||
                table_blocks > mounted.blocks - inode_table) {
                return EXT4_FORMAT;
            }
            mounted.inode_tables[i] = inode_table;
        }
    }

    struct inode root;
    enum ext4_result result = load_inode(&mounted, EXT4_ROOT_INODE, &root);
    if (result != EXT4_OK) {
        return result;
    }
    if (root.type != EXT4_DIRECTORY || !root.size || root.size % EXT4_BLOCK) {
        return EXT4_FORMAT;
    }
    *fs = mounted;
    return EXT4_OK;
}

enum ext4_result ext4_mount(struct ext4 *fs, const void *image, size_t bytes)
{
    enum ext4_result result;

    if (ext4_backed(fs)) {
        return EXT4_BUSY;
    }
    if (!image || bytes < 2 * EXT4_BLOCK) {
        return EXT4_BAD_ARG;
    }
    *fs = (struct ext4){.image = image};
    result = mount_super(fs, (const uint8_t *)image + SUPER_OFFSET, bytes / EXT4_BLOCK);
    if (result != EXT4_OK) {
        *fs = (struct ext4){0};
    }
    return result;
}

enum ext4_result ext4_mount_blk(struct ext4 *fs, const struct ext4_blk_io *io,
                                uint64_t capacity_blocks)
{
    uint8_t b0[EXT4_BLOCK];
    enum ext4_result result;

    if (ext4_backed(fs)) {
        return EXT4_BUSY;
    }
    if (!io || !io->read_block || capacity_blocks < 2) {
        return EXT4_BAD_ARG;
    }
    *fs = (struct ext4){.io = io};
    if (load_block(fs, 0, b0) != EXT4_OK) {
        *fs = (struct ext4){0};
        return EXT4_FORMAT;
    }
    result = mount_super(fs, b0 + SUPER_OFFSET, capacity_blocks);
    if (result != EXT4_OK) {
        *fs = (struct ext4){0};
    }
    return result;
}

void ext4_unmount(struct ext4 *fs)
{
    *fs = (struct ext4){0};
}

/*
 * 把文件逻辑块映射到镜像块。
 * *data == NULL 表示空洞或未初始化 extent；返回错误表示元数据/IO 损坏，
 * 调用方不得把错误当空洞静默填零。
 */
static enum ext4_result map_file_block(const struct ext4 *fs, const struct inode *inode,
                                       uint64_t logical, const uint8_t **data)
{
    *data = NULL;

    if (inode->depth == 0) {
        for (unsigned i = 0; i < inode->count; i++) {
            const struct extent *e = &inode->extents[i];

            if (logical < e->logical || logical - e->logical >= e->count)
                continue;
            if (!e->initialized)
                return EXT4_OK;
            const uint8_t *source =
                block((struct ext4 *)fs, e->physical + (logical - e->logical));
            if (!source)
                return EXT4_FORMAT;
            *data = source;
            return EXT4_OK;
        }
        return EXT4_OK;
    }

    for (unsigned i = 0; i < inode->count; i++) {
        uint64_t low = inode->indexes[i].logical;
        uint64_t high = (i + 1 < inode->count) ? inode->indexes[i + 1].logical
                                              : UINT64_MAX;

        if (logical < low || logical >= high)
            continue;
        /*
         * leaf 指向 fs->cache_blk；下面取数据块时 block() 会覆盖同一
         * cache，因此在返回数据指针前先把 leaf 里的字段全部读完。
         */
        const uint8_t *leaf = block((struct ext4 *)fs, inode->indexes[i].leaf);
        if (leaf == NULL)
            return EXT4_FORMAT;
        uint16_t entries = le16(leaf + 2);
        uint16_t max = le16(leaf + 4);

        if (le16(leaf) != EXTENT_MAGIC || le16(leaf + 6) != 0 ||
            entries > max || max > EXTENT_LEAF_MAX) {
            return EXT4_FORMAT;
        }
        for (unsigned j = 0; j < entries; j++) {
            const uint8_t *entry = leaf + EXTENT_HEAD_LEN + EXTENT_ENTRY_LEN * j;
            uint32_t extent_logical = le32(entry);
            uint16_t raw_len = le16(entry + 4);
            uint32_t length =
                raw_len > EXTENT_UNWRITTEN ? raw_len - EXTENT_UNWRITTEN : raw_len;
            uint64_t physical = (uint64_t)le16(entry + 6) << 32 | le32(entry + 8);

            if (logical < extent_logical || logical - extent_logical >= length)
                continue;
            if (raw_len > EXTENT_UNWRITTEN)
                return EXT4_OK;
            if (physical >= fs->blocks)
                return EXT4_FORMAT;
            const uint8_t *source =
                block((struct ext4 *)fs, physical + (logical - extent_logical));
            if (!source)
                return EXT4_FORMAT;
            *data = source;
            return EXT4_OK;
        }
        /* 落在该叶块负责的逻辑区间但未被 extent 覆盖：空洞。 */
        return EXT4_OK;
    }
    return EXT4_OK;
}

enum ext4_result ext4_lookup(const struct ext4 *fs, uint32_t dir, const char *name,
                            size_t name_len, struct ext4_node *target)
{
    if (!ext4_backed(fs)) {
        return EXT4_NOT_MOUNTED;
    }
    if (!name || !target || !name_len || name_len > EXT4_NAME_MAX)
        return EXT4_BAD_ARG;
    struct inode inode;
    enum ext4_result result = load_inode(fs, dir, &inode);
    if (result != EXT4_OK)
        return result;
    if (inode.type != EXT4_DIRECTORY)
        return EXT4_WRONG_TYPE;
    if (!inode.size || inode.size % EXT4_BLOCK)
        return EXT4_FORMAT;

    for (uint64_t logical = 0; logical < inode.size / EXT4_BLOCK; logical++) {
        const uint8_t *data = NULL;
        enum ext4_result mapped = map_file_block(fs, &inode, logical, &data);
        if (mapped != EXT4_OK)
            return mapped;
        if (!data)
            return EXT4_FORMAT;
        for (size_t pos = 0; pos < EXT4_BLOCK;) {
            const uint8_t *entry = data + pos;
            size_t remaining = EXT4_BLOCK - pos;
            if (remaining < 8)
                return EXT4_FORMAT;
            uint32_t ino = le32(entry);
            uint16_t rec_len = le16(entry + 4);
            uint8_t len = entry[6];
            if (rec_len < 8 || rec_len > remaining || (rec_len & 3) || len > rec_len - 8 ||
                (ino && (!len || ino > fs->inodes)))
                return EXT4_FORMAT;
            if (ino && len == name_len && !memcmp(entry + 8, name, len)) {
                struct inode child;
                result = load_inode(fs, ino, &child);
                if (result == EXT4_OK)
                    *target = (struct ext4_node){.ino = ino, .type = child.type};
                return result;
            }
            pos += rec_len;
        }
    }
    return EXT4_NOT_FOUND;
}

enum ext4_result ext4_read_at(const struct ext4 *fs, uint32_t ino, uint64_t offset,
                             void *dst, size_t len, size_t *read)
{
    if (!ext4_backed(fs)) {
        return EXT4_NOT_MOUNTED;
    }
    if (!read || (len && !dst))
        return EXT4_BAD_ARG;
    struct inode inode;
    enum ext4_result result = load_inode(fs, ino, &inode);
    if (result != EXT4_OK)
        return result;
    if (inode.type != EXT4_FILE)
        return EXT4_WRONG_TYPE;
    if (offset >= inode.size)
        len = 0;
    else if (len > inode.size - offset)
        len = inode.size - offset;

    size_t copied = 0;
    while (copied < len) {
        size_t within = offset % EXT4_BLOCK;
        size_t count = EXT4_BLOCK - within;
        if (count > len - copied)
            count = len - copied;
        const uint8_t *source = NULL;
        enum ext4_result mapped =
            map_file_block(fs, &inode, offset / EXT4_BLOCK, &source);
        if (mapped != EXT4_OK)
            return mapped;
        if (source)
            memcpy((uint8_t *)dst + copied, source + within, count);
        else
            memset((uint8_t *)dst + copied, 0, count);
        offset += count;
        copied += count;
    }
    *read = copied;
    return EXT4_OK;
}