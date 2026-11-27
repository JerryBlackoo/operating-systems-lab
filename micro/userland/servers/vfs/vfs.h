#ifndef MICRO_MODEL_VFS_H
#define MICRO_MODEL_VFS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum vfs_result {
    VFS_OK, VFS_BAD_PATH, VFS_NOT_FOUND, VFS_NOT_DIR, VFS_NOT_FILE,
    VFS_BAD_FD, VFS_NO_SPACE, VFS_IO_ERROR, VFS_BAD_ARG,
};
enum vfs_type { VFS_DIRECTORY, VFS_REGULAR };
enum {
    VFS_CLIENTS = 4, VFS_FILES = 8, VFS_NAME_MAX = 255,
    /* 目录项缓存槽数。只影响命中率，不影响语义：任何时刻清空都只是变慢。 */
    VFS_DENTRIES = 32,
    /* 能被缓存的分量名长度上限。更长的名字照常解析，只是不进缓存。 */
    VFS_DENTRY_NAME = 32,
};

struct vfs_node { uint32_t inode; enum vfs_type type; };
struct vfs_port {
    void *ctx;
    enum vfs_result (*lookup)(void *, uint32_t dir, const char *, size_t, struct vfs_node *);
    enum vfs_result (*retain)(void *, uint32_t inode);
    void (*release)(void *, uint32_t inode);
    enum vfs_result (*read_at)(void *, uint32_t inode, uint64_t offset, void *, size_t, size_t *);
};

/*
 * 目录项缓存（dentry）：把一次 (parent_inode, name) 的解析结果留在内存里，
 * 供后续 resolve 直接命中，省掉「读 inode 表块 + 扫目录数据块 + 再读 inode」。
 *
 * 条目本身不持有 inode 引用（本模型的 retain/release 不计数），因此淘汰或
 * 清空随时安全。negative 条目记录「这个名字在那个目录下不存在」。
 */
struct vfs_dentry {
    uint32_t parent;   /* 父目录 inode */
    uint32_t inode;    /* 命中目标；negative 条目为 0 */
    uint32_t stamp;    /* LRU 时钟，越大越新 */
    uint8_t name_len;
    bool used;
    bool negative;
    enum vfs_type type;
    char name[VFS_DENTRY_NAME]; /* 定长、不带 '\0'；比较用 name_len */
};

/* 只增不减的计数器（flush 也不清零），用于对照实验而非调度。 */
struct vfs_dcache_counters {
    unsigned hits, misses, negative_hits, evictions, bypass;
};

/* FD : (client, fd) ⇀ (inode, offset)。每个客户端一张定长描述符表，
   槽位按 fd 下标直接索引，关闭最后一个描述符时释放客户端记录。 */
struct vfs_file { uint32_t inode; uint64_t offset; bool used; };
struct vfs_client { uint32_t id; unsigned open; struct vfs_file files[VFS_FILES]; };
struct vfs {
    const struct vfs_port *port;
    uint32_t root;
    struct vfs_client clients[VFS_CLIENTS];
    struct vfs_dentry dentries[VFS_DENTRIES];
    uint32_t dentry_stamp;
    unsigned dentry_used;
    struct vfs_dcache_counters dcache;
};

enum vfs_result vfs_open(struct vfs *, uint32_t client, const char *path, unsigned *fd);
enum vfs_result vfs_read(struct vfs *, uint32_t client, unsigned fd, void *, size_t, size_t *);
/* pread：从显式 offset 读取，不推进描述符 offset。 */
enum vfs_result vfs_read_at(struct vfs *, uint32_t client, unsigned fd, uint64_t offset,
                            void *, size_t, size_t *);
enum vfs_result vfs_close(struct vfs *, uint32_t client, unsigned fd);

/*
 * 丢弃全部目录项缓存。只读模型下不需要调用；一旦加入写路径（create/mkdir/
 * unlink/rename），任何目录内容变化后必须调用，否则负缓存会掩盖新建的文件。
 * 计数器不清零，便于观察「清空后重新填充」的代价。
 */
void vfs_dcache_flush(struct vfs *);
/* 只丢弃某一父目录下的条目。写路径的最小失效粒度。 */
void vfs_dcache_invalidate_parent(struct vfs *, uint32_t parent);

#endif
