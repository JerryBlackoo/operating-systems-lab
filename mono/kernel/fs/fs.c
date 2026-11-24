#include "fs/fs.h"

#include "fs/bio.h"
#include "lib/kprintf.h"
#include "lib/string.h"

#define SUPER_BLOCK 0
#define INODE_BITMAP_BLOCK 1
#define DATA_BITMAP_BLOCK 2
#define INODE_TABLE_BLOCK 3
#define DATA_START_BLOCK 4

struct fs_super {
    uint32_t magic;
    uint32_t nblocks;
    uint32_t ninodes;
    uint32_t inode_bitmap;
    uint32_t data_bitmap;
    uint32_t inode_start;
    uint32_t data_start;
};

static struct fs_super super;
static struct file files[FS_MAX_FILES];

static int bit_test(const uint8_t *map, uint32_t bit)
{
    return (map[bit / 8] >> (bit % 8)) & 1;
}

static void bit_set(uint8_t *map, uint32_t bit)
{
    map[bit / 8] |= (uint8_t)(1u << (bit % 8));
}

static void inode_read(uint32_t ino, struct fs_inode *out)
{
    struct buf *b;

    /* inode 号来自目录项；目录数据可被用户写入，不能越界访问 inode 表。 */
    if (ino >= FS_MAX_INODES) {
        memset(out, 0, sizeof(*out));
        return;
    }
    b = bread(INODE_TABLE_BLOCK);
    memcpy(out, b->data + ino * sizeof(struct fs_inode), sizeof(*out));
    brelse(b);
}

static void inode_write(uint32_t ino, const struct fs_inode *in)
{
    struct buf *b;

    if (ino >= FS_MAX_INODES) {
        return;
    }
    b = bread(INODE_TABLE_BLOCK);
    memcpy(b->data + ino * sizeof(struct fs_inode), in, sizeof(*in));
    bwrite(b);
    brelse(b);
}

static int inode_alloc(int type)
{
    struct buf *b = bread(INODE_BITMAP_BLOCK);
    int ino = -1;

    for (uint32_t i = 0; i < super.ninodes; i++) {
        if (bit_test(b->data, i) == 0) {
            bit_set(b->data, i);
            bwrite(b);
            ino = (int)i;
            break;
        }
    }
    brelse(b);

    if (ino >= 0) {
        struct fs_inode in;
        memset(&in, 0, sizeof(in));
        in.type = (uint16_t)type;
        in.nlink = 1;
        inode_write((uint32_t)ino, &in);
    }
    return ino;
}

static int block_alloc(void)
{
    struct buf *b = bread(DATA_BITMAP_BLOCK);
    int block = -1;

    for (uint32_t i = 0; i < super.nblocks - super.data_start; i++) {
        if (bit_test(b->data, i) == 0) {
            bit_set(b->data, i);
            bwrite(b);
            block = (int)(super.data_start + i);
            break;
        }
    }
    brelse(b);

    if (block >= 0) {
        struct buf *z = bread((uint32_t)block);
        memset(z->data, 0, BLOCK_SIZE);
        bwrite(z);
        brelse(z);
    }
    return block;
}

static void block_free(uint32_t block)
{
    struct buf *b;
    uint32_t bit;

    if (block < super.data_start || block >= super.nblocks) {
        return;
    }
    b = bread(DATA_BITMAP_BLOCK);
    bit = block - super.data_start;
    b->data[bit / 8] &= (uint8_t)~(1u << (bit % 8));
    bwrite(b);
    brelse(b);
}

int fs_read(int ino, uint32_t off, void *dst, uint32_t len)
{
    struct fs_inode in;
    uint8_t *out = (uint8_t *)dst;
    uint32_t done = 0;

    inode_read((uint32_t)ino, &in);
    if (off >= in.size) {
        return 0;
    }
    if (off + len > in.size) {
        len = in.size - off;
    }

    while (done < len) {
        uint32_t block_index = (off + done) / BLOCK_SIZE;
        uint32_t block_off = (off + done) % BLOCK_SIZE;
        uint32_t n = len - done;

        if (block_index >= 8) {
            break;
        }
        if (n > BLOCK_SIZE - block_off) {
            n = BLOCK_SIZE - block_off;
        }
        if (in.direct[block_index] == 0) {
            /* 多 fd 场景下 O_TRUNC 后另一 fd 从高偏移继续写会留下洞；
             * 洞按零页读取，而不是当成 EOF。 */
            memset(out + done, 0, n);
            done += n;
            continue;
        }
        struct buf *b = bread(in.direct[block_index]);
        memcpy(out + done, b->data + block_off, n);
        brelse(b);
        done += n;
    }
    return (int)done;
}

int fs_write(int ino, uint32_t off, const void *src, uint32_t len)
{
    struct fs_inode in;
    const uint8_t *input = (const uint8_t *)src;
    uint32_t done = 0;
    int allocated = 0;

    inode_read((uint32_t)ino, &in);
    while (done < len) {
        uint32_t block_index = (off + done) / BLOCK_SIZE;
        uint32_t block_off = (off + done) % BLOCK_SIZE;
        uint32_t n = len - done;

        if (block_index >= 8) {
            break;
        }
        if (in.direct[block_index] == 0) {
            int block = block_alloc();
            if (block < 0) {
                break;
            }
            in.direct[block_index] = (uint32_t)block;
            allocated = 1;
        }
        if (n > BLOCK_SIZE - block_off) {
            n = BLOCK_SIZE - block_off;
        }
        struct buf *b = bread(in.direct[block_index]);
        memcpy(b->data + block_off, input + done, n);
        bwrite(b);
        brelse(b);
        done += n;
    }

    /* 实际写入后回写 inode：文件变大，或本次为已有文件内的“洞”
     * 分配了新块（direct[] 变化）时都必须持久化。只按大小判断会
     * 丢掉新块号并泄漏该块。 */
    if (done > 0 && (allocated != 0 || off + done > in.size)) {
        if (off + done > in.size) {
            in.size = off + done;
        }
        inode_write((uint32_t)ino, &in);
    }
    return (int)done;
}

int fs_truncate(int ino)
{
    struct fs_inode in;

    inode_read((uint32_t)ino, &in);
    if (in.type != 1) {
        return -1;
    }
    for (int i = 0; i < 8; i++) {
        if (in.direct[i] != 0) {
            block_free(in.direct[i]);
            in.direct[i] = 0;
        }
    }
    in.size = 0;
    inode_write((uint32_t)ino, &in);
    return 0;
}

int fs_readdir(int ino, uint32_t index, struct fs_dirent *out)
{
    struct fs_inode in;

    inode_read((uint32_t)ino, &in);
    if (in.type != 2 || index * sizeof(*out) >= in.size) {
        return -1;
    }
    return fs_read(ino, index * sizeof(*out), out, sizeof(*out)) == (int)sizeof(*out) ? 0 : -1;
}

int fs_is_dir(int ino)
{
    struct fs_inode in;

    inode_read((uint32_t)ino, &in);
    return in.type == 2;
}

static int dir_lookup(int dir, const char *name)
{
    struct fs_dirent de;
    struct fs_inode in;

    inode_read((uint32_t)dir, &in);
    for (uint32_t index = 0; index * sizeof(de) < in.size; index++) {
        if (fs_readdir(dir, index, &de) != 0) {
            break;
        }
        if (strcmp(de.name, name) == 0) {
            return de.ino < FS_MAX_INODES ? (int)de.ino : -1;
        }
    }
    return -1;
}

static int dir_add(int dir, const char *name, uint32_t ino)
{
    struct fs_inode in;
    struct fs_dirent de;

    inode_read((uint32_t)dir, &in);
    memset(&de, 0, sizeof(de));
    strncpy(de.name, name, sizeof(de.name) - 1);
    de.ino = ino;
    return fs_write(dir, in.size, &de, sizeof(de)) == (int)sizeof(de) ? 0 : -1;
}

int fs_lookup(const char *path)
{
    int current_ino = 0;
    const char *p = path;
    int trailing_slash = 0;

    if (path == 0 || path[0] != '/') {
        return -1;
    }
    while (*p != '\0') {
        char name[28];
        int n = 0;

        while (*p == '/') {
            p++;
        }
        if (*p == '\0') {
            trailing_slash = 1;
            break;
        }
        while (*p != '\0' && *p != '/' && n < (int)sizeof(name) - 1) {
            name[n++] = *p++;
        }
        /* 单个路径分量超长：直接拒绝，避免截断后匹配到另一个文件。 */
        if (*p != '\0' && *p != '/') {
            return -1;
        }
        name[n] = '\0';
        current_ino = dir_lookup(current_ino, name);
        if (current_ino < 0) {
            return -1;
        }
    }
    /* 尾随 '/' 只能用于目录；"/motd/" 不能解析成普通文件。 */
    if (trailing_slash != 0 && fs_is_dir(current_ino) == 0) {
        return -1;
    }
    return current_ino;
}

static int split_parent(const char *path, char *parent, char *name)
{
    const char *last = 0;
    uint64_t len = strlen(path);

    for (uint64_t i = 0; i < len; i++) {
        if (path[i] == '/') {
            last = &path[i];
        }
    }
    if (last == 0 || last[1] == '\0') {
        return -1;
    }
    uint64_t parent_len = (uint64_t)(last - path);
    if (parent_len == 0) {
        parent[0] = '/';
        parent[1] = '\0';
    } else {
        if (parent_len >= 64) {
            return -1;
        }
        memcpy(parent, path, parent_len);
        parent[parent_len] = '\0';
    }
    /* 目录项名字段固定 28 字节（含结尾 '\0'），超长名字直接拒绝，
     * 否则截断后可能与已有目录项重名。 */
    if (strlen(last + 1) >= sizeof(((struct fs_dirent *)0)->name)) {
        return -1;
    }
    strncpy(name, last + 1, 27);
    name[27] = '\0';
    return 0;
}

static void inode_free(uint32_t ino)
{
    struct buf *b = bread(INODE_BITMAP_BLOCK);

    b->data[ino / 8] &= (uint8_t)~(1u << (ino % 8));
    bwrite(b);
    brelse(b);
}

int fs_create(const char *path, int type)
{
    char parent[64];
    char name[28];
    int parent_ino;
    int ino;

    ino = fs_lookup(path);
    if (ino >= 0) {
        struct fs_inode existing;
        inode_read((uint32_t)ino, &existing);
        return existing.type == type ? ino : -1;
    }
    if (split_parent(path, parent, name) != 0) {
        return -1;
    }
    parent_ino = fs_lookup(parent);
    if (parent_ino < 0 || fs_is_dir(parent_ino) == 0) {
        return -1;
    }

    ino = inode_alloc(type);
    if (ino < 0) {
        return -1;
    }
    if (dir_add(parent_ino, name, (uint32_t)ino) != 0) {
        inode_free((uint32_t)ino);
        return -1;
    }
    return ino;
}

int fs_mkdir(const char *path)
{
    /* mkdir 的语义是「新建」：已存在（目录或文件）都应失败，
     * 不能复用 fs_create 对 O_CREATE 的幂等行为。 */
    if (fs_lookup(path) >= 0) {
        return -1;
    }
    return fs_create(path, 2);
}

void fs_init(void)
{
    struct buf *b;

    memset(&super, 0, sizeof(super));
    super.magic = FS_MAGIC;
    super.nblocks = RAMDISK_BLOCKS;
    super.ninodes = FS_MAX_INODES;
    super.inode_bitmap = INODE_BITMAP_BLOCK;
    super.data_bitmap = DATA_BITMAP_BLOCK;
    super.inode_start = INODE_TABLE_BLOCK;
    super.data_start = DATA_START_BLOCK;

    b = bread(SUPER_BLOCK);
    memset(b->data, 0, BLOCK_SIZE);
    memcpy(b->data, &super, sizeof(super));
    bwrite(b);
    brelse(b);

    b = bread(INODE_BITMAP_BLOCK);
    memset(b->data, 0, BLOCK_SIZE);
    bwrite(b);
    brelse(b);

    b = bread(DATA_BITMAP_BLOCK);
    memset(b->data, 0, BLOCK_SIZE);
    bwrite(b);
    brelse(b);

    b = bread(INODE_TABLE_BLOCK);
    memset(b->data, 0, BLOCK_SIZE);
    bwrite(b);
    brelse(b);

    /* root inode */
    b = bread(INODE_BITMAP_BLOCK);
    bit_set(b->data, 0);
    bwrite(b);
    brelse(b);

    struct fs_inode root;
    memset(&root, 0, sizeof(root));
    root.type = 2;
    root.nlink = 1;
    inode_write(0, &root);

    int motd = fs_create("/motd", 1);
    if (motd >= 0) {
        static const char text[] =
            "Welcome to operating-systems-lab\n"
            "This is a teaching OS on RISC-V and ARM Zynq-7010.\n";
        fs_write(motd, 0, text, sizeof(text) - 1);
    }
    fs_mkdir("/tmp");
    kprintf("[fs] formatted: %d blocks, %d inodes, root ready\n",
            RAMDISK_BLOCKS, FS_MAX_INODES);
}

void fs_selftest(void)
{
    char buf[128];
    int ino = fs_lookup("/motd");
    int n;

    if (ino < 0) {
        kprintf("[fs] /motd missing\n");
        return;
    }
    n = fs_read(ino, 0, buf, sizeof(buf) - 1);
    buf[n > 0 ? n : 0] = '\0';
    kprintf("[fs] /motd: %s", buf);

    ino = fs_create("/tmp/note", 1);
    if (ino >= 0 && fs_write(ino, 0, "hello", 5) == 5) {
        char tmp[8] = {0};
        fs_read(ino, 0, tmp, 5);
        kprintf("[fs] rw test: %s\n", tmp);
    }
}

struct file *fs_file_alloc(void)
{
    for (int i = 0; i < FS_MAX_FILES; i++) {
        if (files[i].used == 0) {
            memset(&files[i], 0, sizeof(files[i]));
            files[i].used = 1;
            files[i].refs = 1;
            return &files[i];
        }
    }
    return 0;
}

struct file *fs_file_at(int index)
{
    if (index < 0 || index >= FS_MAX_FILES) {
        return 0;
    }
    return &files[index];
}

int fs_file_index(struct file *f)
{
    return (int)(f - files);
}

void fs_file_dup(struct file *f)
{
    if (f != 0 && f->used != 0) {
        f->refs++;
    }
}

void fs_file_free(struct file *f)
{
    if (f == 0 || f->used == 0) {
        return;
    }
    if (--f->refs <= 0) {
        f->used = 0;
        f->refs = 0;
    }
}
