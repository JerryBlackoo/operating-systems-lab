#include "ext4.h"
#include "demo.h"

enum {
    SUPER_OFFSET = 1024, EXT4_MAGIC = 0xef53, GROUP_DESC_SIZE = 32,
    FEATURE_FILETYPE = 0x2, FEATURE_EXTENTS = 0x40, RO_FEATURES = 0x43,
    INODE_EXTENTS = 0x80000, EXTENT_MAGIC = 0xf30a, EXTENT_ENTRIES = 4,
    TYPE_MASK = 0xf000, MODE_DIR = 0x4000, MODE_FILE = 0x8000,
};

struct extent {
    uint32_t logical;
    uint64_t physical;
    uint32_t count;
    bool initialized;
};

struct inode {
    enum ext4_type type;
    uint64_t size;
    uint16_t count;
    struct extent extents[EXTENT_ENTRIES];
};

static uint16_t le16(const uint8_t *p)
{
    return (uint16_t)p[0] | (uint16_t)p[1] << 8;
}

static uint32_t le32(const uint8_t *p)
{
    return (uint32_t)le16(p) | (uint32_t)le16(p + 2) << 16;
}

static const uint8_t *block(const struct ext4 *fs, uint64_t number)
{
    return fs->image + number * EXT4_BLOCK;
}

static enum ext4_result load_inode(const struct ext4 *fs, uint32_t ino, struct inode *inode)
{
    if (!ino || ino > fs->inodes)
        return EXT4_NOT_FOUND;
    uint32_t group = (ino - 1) / fs->inodes_per_group;
    uint32_t slot = (ino - 1) % fs->inodes_per_group;
    const uint8_t *p = block(fs, fs->inode_tables[group]) + (size_t)slot * fs->inode_size;
    uint16_t mode = le16(p) & TYPE_MASK;
    if (mode != MODE_DIR && mode != MODE_FILE)
        return EXT4_WRONG_TYPE;
    if (le32(p + 0x20) != INODE_EXTENTS)
        return EXT4_UNSUPPORTED;
    inode->type = mode == MODE_DIR ? EXT4_DIRECTORY : EXT4_FILE;
    inode->size = (uint64_t)le32(p + 0x6c) << 32 | le32(p + 4);

    const uint8_t *ext = p + 0x28;
    uint16_t count = le16(ext + 2);
    if (le16(ext) != EXTENT_MAGIC || count > le16(ext + 4) || le16(ext + 4) > EXTENT_ENTRIES)
        return EXT4_FORMAT;
    if (le16(ext + 6) != 0)
        return EXT4_UNSUPPORTED;
    inode->count = count;
    uint64_t end = 0;
    for (unsigned i = 0; i < count; i++) {
        const uint8_t *entry = ext + 12 + 12 * i;
        uint16_t length = le16(entry + 4);
        struct extent e = {
            .logical = le32(entry),
            .physical = (uint64_t)le16(entry + 6) << 32 | le32(entry + 8),
            .count = length > 0x8000 ? length - 0x8000u : length,
            .initialized = length <= 0x8000,
        };
        if (e.logical < end || !e.count || !e.physical ||
            e.physical >= fs->blocks || e.count > fs->blocks - e.physical)
            return EXT4_FORMAT;
        end = (uint64_t)e.logical + e.count;
        if (end > (UINT64_C(1) << 32))
            return EXT4_FORMAT;
        inode->extents[i] = e;
    }
    return EXT4_OK;
}

enum ext4_result ext4_mount(struct ext4 *fs, const void *image, size_t bytes)
{
    if (fs->image)
        return EXT4_BUSY;
    if (!image || bytes < 2 * EXT4_BLOCK)
        return EXT4_BAD_ARG;
    const uint8_t *super = (const uint8_t *)image + SUPER_OFFSET;
    if (le16(super + 0x38) != EXT4_MAGIC || le16(super + 0x3a) != 1)
        return EXT4_FORMAT;
    if (le32(super + 0x18) != 2 || le32(super + 0x14) != 0 ||
        le32(super + 0x60) != (FEATURE_FILETYPE | FEATURE_EXTENTS) ||
        (le32(super + 0x64) & ~RO_FEATURES))
        return EXT4_UNSUPPORTED;

    struct ext4 mounted = {
        .image = image, .blocks = le32(super + 4), .inodes = le32(super),
        .inodes_per_group = le32(super + 0x28), .inode_size = le16(super + 0x58),
    };
    uint32_t blocks_per_group = le32(super + 0x20);
    if (mounted.blocks < 2 || mounted.blocks > bytes / EXT4_BLOCK ||
        !blocks_per_group || blocks_per_group > 8 * EXT4_BLOCK ||
        !mounted.inodes_per_group || mounted.inodes_per_group > 8 * EXT4_BLOCK ||
        mounted.inodes < EXT4_ROOT_INODE || mounted.inode_size < 128 ||
        mounted.inode_size > EXT4_BLOCK || (mounted.inode_size & (mounted.inode_size - 1)))
        return EXT4_FORMAT;
    unsigned groups = 1 + (mounted.blocks - 1) / blocks_per_group;
    if (groups > EXT4_GROUPS)
        return EXT4_UNSUPPORTED;
    if (mounted.inodes > groups * mounted.inodes_per_group)
        return EXT4_FORMAT;

    size_t table_size = (size_t)mounted.inodes_per_group * mounted.inode_size;
    size_t table_blocks = (table_size + EXT4_BLOCK - 1) / EXT4_BLOCK;
    for (unsigned i = 0; i < groups; i++) {
        const uint8_t *desc = block(&mounted, 1) + i * GROUP_DESC_SIZE;
        uint32_t inode_table = le32(desc + 8);
        if (inode_table <= 1 || inode_table >= mounted.blocks ||
            table_blocks > mounted.blocks - inode_table)
            return EXT4_FORMAT;
        mounted.inode_tables[i] = inode_table;
    }
    struct inode root;
    enum ext4_result result = load_inode(&mounted, EXT4_ROOT_INODE, &root);
    if (result != EXT4_OK)
        return result;
    if (root.type != EXT4_DIRECTORY || !root.size || root.size % EXT4_BLOCK)
        return EXT4_FORMAT;
    *fs = mounted;
    return EXT4_OK;
}

void ext4_unmount(struct ext4 *fs)
{
    *fs = (struct ext4){0};
}

static const uint8_t *file_block(const struct ext4 *fs, const struct inode *inode, uint64_t logical)
{
    for (unsigned i = 0; i < inode->count; i++) {
        const struct extent *e = &inode->extents[i];
        if (logical >= e->logical && logical - e->logical < e->count)
            return e->initialized ? block(fs, e->physical + (logical - e->logical)) : NULL;
    }
    return NULL;
}

enum ext4_result ext4_lookup(const struct ext4 *fs, uint32_t dir, const char *name,
                            size_t name_len, struct ext4_node *target)
{
    if (!fs->image)
        return EXT4_NOT_MOUNTED;
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
        const uint8_t *data = file_block(fs, &inode, logical);
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
    if (!fs->image)
        return EXT4_NOT_MOUNTED;
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
        const uint8_t *source = file_block(fs, &inode, offset / EXT4_BLOCK);
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
