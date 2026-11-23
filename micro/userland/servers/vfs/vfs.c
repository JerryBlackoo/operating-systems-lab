#include "vfs.h"

static enum vfs_result lookup(struct vfs *vfs, const char *name, struct vfs_node *node)
{
    *node = (struct vfs_node){.inode = vfs->root, .type = VFS_DIRECTORY};
    for (;;) {
        const char *end = name;
        while (*end && *end != '/')
            end++;
        size_t len = (size_t)(end - name);
        if (!len || len > VFS_NAME_MAX ||
            (name[0] == '.' && (len == 1 || (len == 2 && name[1] == '.'))))
            return VFS_BAD_PATH;
        enum vfs_result result = vfs->port->lookup(vfs->port->ctx, node->inode, name, len, node);
        if (result != VFS_OK)
            return result;
        if (!*end)
            return node->type == VFS_REGULAR ? VFS_OK : VFS_NOT_FILE;
        if (node->type != VFS_DIRECTORY)
            return VFS_NOT_DIR;
        name = end + 1;
    }
}

enum vfs_result vfs_open(struct vfs *vfs, uint32_t client, const char *path, unsigned *fd)
{
    if (!client || !fd)
        return VFS_BAD_ARG;
    if (!path || path[0] != '/')
        return VFS_BAD_PATH;
    if (!path[1])
        return VFS_NOT_FILE;
    struct vfs_node node;
    enum vfs_result result = lookup(vfs, path + 1, &node);
    if (result != VFS_OK)
        return result;
    for (unsigned i = 0; i < VFS_FILES; i++) {
        if (!vfs->files[i].client) {
            result = vfs->port->retain(vfs->port->ctx, node.inode);
            if (result != VFS_OK)
                return result;
            vfs->files[i] = (struct vfs_file){.client = client, .inode = node.inode};
            *fd = i;
            return VFS_OK;
        }
    }
    return VFS_NO_SPACE;
}

static struct vfs_file *get_file(struct vfs *vfs, uint32_t client, unsigned fd)
{
    if (!client || fd >= VFS_FILES || vfs->files[fd].client != client)
        return NULL;
    return &vfs->files[fd];
}

enum vfs_result vfs_read(struct vfs *vfs, uint32_t client, unsigned fd, void *buffer,
                         size_t count, size_t *actual)
{
    struct vfs_file *file = get_file(vfs, client, fd);
    if (!file)
        return VFS_BAD_FD;
    if (!actual || (count && !buffer))
        return VFS_BAD_ARG;
    if (!count) {
        *actual = 0;
        return VFS_OK;
    }
    size_t n;
    enum vfs_result result = vfs->port->read_at(vfs->port->ctx, file->inode, file->offset,
                                               buffer, count, &n);
    if (result != VFS_OK)
        return result;
    if (n > count || n > UINT64_MAX - file->offset)
        return VFS_IO_ERROR;
    file->offset += n;
    *actual = n;
    return VFS_OK;
}

enum vfs_result vfs_close(struct vfs *vfs, uint32_t client, unsigned fd)
{
    struct vfs_file *file = get_file(vfs, client, fd);
    if (!file)
        return VFS_BAD_FD;
    uint32_t inode = file->inode;
    *file = (struct vfs_file){0};
    vfs->port->release(vfs->port->ctx, inode);
    return VFS_OK;
}
