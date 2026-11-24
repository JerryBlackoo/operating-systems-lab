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
enum { VFS_CLIENTS = 4, VFS_FILES = 8, VFS_NAME_MAX = 255 };

struct vfs_node { uint32_t inode; enum vfs_type type; };
struct vfs_port {
    void *ctx;
    enum vfs_result (*lookup)(void *, uint32_t dir, const char *, size_t, struct vfs_node *);
    enum vfs_result (*retain)(void *, uint32_t inode);
    void (*release)(void *, uint32_t inode);
    enum vfs_result (*read_at)(void *, uint32_t inode, uint64_t offset, void *, size_t, size_t *);
};

/* FD : (client, fd) ⇀ (inode, offset)。每个客户端一张定长描述符表，
   槽位按 fd 下标直接索引，关闭最后一个描述符时释放客户端记录。 */
struct vfs_file { uint32_t inode; uint64_t offset; bool used; };
struct vfs_client { uint32_t id; unsigned open; struct vfs_file files[VFS_FILES]; };
struct vfs {
    const struct vfs_port *port;
    uint32_t root;
    struct vfs_client clients[VFS_CLIENTS];
};

enum vfs_result vfs_open(struct vfs *, uint32_t client, const char *path, unsigned *fd);
enum vfs_result vfs_read(struct vfs *, uint32_t client, unsigned fd, void *, size_t, size_t *);
enum vfs_result vfs_close(struct vfs *, uint32_t client, unsigned fd);

#endif
